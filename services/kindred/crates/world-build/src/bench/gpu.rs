//! The batched path's device on an NVIDIA GPU (`bench --device cuda`, feature `gpu`): the
//! primitives of [`Device`] as NVRTC kernels, cuBLAS (the Hessians `I + M M^T` as one strided
//! batched SGEMM, `mu` of every candidate as SGEMM) and cuSOLVER (batched Cholesky
//! `potrfBatched` and solve `potrsBatched`). Precisions as [`super::batch`] says: f64 objective,
//! gradient and `w`; f32 preconditioned Hessian and factor. f64 runs at 1/32 of f32 or less on
//! these consumer cards, but only the O(terms · d) work is f64; the O(terms · d²) Hessian and the
//! O(d³) factor are f32.
//!
//! The CUDA libraries are loaded at run time (cudarc's dynamic loading, CUDA 13.0 as the
//! image's runtime); nothing is linked at build time, so the binary builds and runs without a
//! GPU and [`Cuda::new`] says why when there is none.

use std::sync::Arc;

use anyhow::Context as _;
use cudarc::cublas::sys::cublasOperation_t;
use cudarc::cublas::{CudaBlas, Gemm as _, GemmConfig, StridedBatchedConfig};
use cudarc::cusolver::{DnHandle, sys as solver};
use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, DevicePtr as _, DevicePtrMut as _,
    LaunchConfig, PushKernelArg as _,
};
use cudarc::nvrtc::{CompileOptions, compile_ptx_with_opts};

use super::batch::{Device, JITTER, JITTER_TRIES, ONE, Terms};

/// Threads per block of the per-reader kernels (BLOCK in the source).
const BLOCK: u32 = 128;
/// The largest vector size the kernels hold in shared memory (MAXD in the source).
const MAXD: usize = 256;
/// Warps per block of the quadratic-form kernel (WARPS in the source).
const WARPS: u32 = 8;
/// Readers whose Hessians are built and factored at once: bounds the scratch (`d * terms` and
/// `d * d` f32 per reader).
const SUB: usize = 512;
/// Rows of one candidate-score SGEMM: bounds its output (`rows * candidates` f32).
const SCORE_ROWS: usize = 256;
// The kernels' ONE is the batch path's.
const _: () = assert!(ONE == 0xffff_ffff);

/// The kernels. Every per-reader kernel takes the list of readers it works on and runs one
/// block per listed reader; `x_n = phi(a_n) - phi(b_n)`, or `phi(a_n)` when `b_n` is ONE.
/// Matrices are column-major, as cuBLAS and cuSOLVER read them.
const SOURCE: &str = r#"
#define BLOCK 128
#define MAXD 256
#define TILE 512
#define WARPS 8
#define ONE 0xffffffffu

__device__ double sigmoid_(double z) {
    if (z >= 0.0) return 1.0 / (1.0 + exp(-z));
    double e = exp(z);
    return e / (1.0 + e);
}

__device__ double ln_sigmoid_(double z) {
    return z >= 0.0 ? -log1p(exp(-z)) : z - log1p(exp(z));
}

__device__ double x_at(const float* phi, int d, unsigned a, unsigned b, int j) {
    double v = (double)phi[(size_t)a * d + j];
    if (b != ONE) v -= (double)phi[(size_t)b * d + j];
    return v;
}

__device__ double xdot(const float* phi, int d, unsigned a, unsigned b, const double* w) {
    double s = 0.0;
    for (int j = 0; j < d; ++j) s += x_at(phi, d, a, b, j) * w[j];
    return s;
}

__device__ double block_sum(double v, double* sh) {
    sh[threadIdx.x] = v;
    __syncthreads();
    for (int s = BLOCK / 2; s > 0; s >>= 1) {
        if (threadIdx.x < s) sh[threadIdx.x] += sh[threadIdx.x + s];
        __syncthreads();
    }
    double r = sh[0];
    __syncthreads();
    return r;
}

__device__ double block_max(double v, double* sh) {
    sh[threadIdx.x] = v;
    __syncthreads();
    for (int s = BLOCK / 2; s > 0; s >>= 1) {
        if (threadIdx.x < s) sh[threadIdx.x] = fmax(sh[threadIdx.x], sh[threadIdx.x + s]);
        __syncthreads();
    }
    double r = sh[0];
    __syncthreads();
    return r;
}

// L(w) = sum_n omega_n ln sigma(z_n) - 0.5 sum_j (w_j - m0_j)^2 / s0_j, at w, or with `trial`
// at w + alpha step (written to w_trial).
extern "C" __global__ void objective(
    int m, const unsigned* list, const double* w, const double* step, const double* alpha,
    int trial, double* w_trial, const unsigned* off, const unsigned* ta, const unsigned* tb,
    const double* tt, const double* ty, const double* ttau, const double* tom,
    const float* phi, const double* m0, const double* s0inv, int d, double* out)
{
    __shared__ double sh[BLOCK];
    __shared__ double wl[MAXD];
    int k = blockIdx.x;
    if (k >= m) return;
    unsigned r = list[k];
    size_t base = (size_t)r * d;
    for (int j = threadIdx.x; j < d; j += BLOCK) {
        double v = w[base + j];
        if (trial) {
            v += alpha[k] * step[base + j];
            w_trial[base + j] = v;
        }
        wl[j] = v;
    }
    __syncthreads();
    double like = 0.0;
    for (unsigned n = off[r] + threadIdx.x; n < off[r + 1]; n += BLOCK) {
        double z = ty[n] * (xdot(phi, d, ta[n], tb[n], wl) - tt[n]) / ttau[n];
        like += tom[n] * ln_sigmoid_(z);
    }
    double pr = 0.0;
    for (int j = threadIdx.x; j < d; j += BLOCK) {
        double dw = wl[j] - m0[j];
        pr += dw * dw * s0inv[j];
    }
    like = block_sum(like, sh);
    pr = block_sum(pr, sh);
    if (threadIdx.x == 0) out[k] = like - 0.5 * pr;
}

// g = sum_n omega_n y_n sigma(-z_n) / tau_n x_n - (w - m0) / s0, its largest absolute entry,
// and each term's sqrt(omega_n sigma(z_n)(1 - sigma(z_n))) / tau_n for the Hessian.
extern "C" __global__ void gradient(
    int m, const unsigned* list, const double* w, const unsigned* off, const unsigned* ta,
    const unsigned* tb, const double* tt, const double* ty, const double* ttau,
    const double* tom, const float* phi, const double* m0, const double* s0inv, int d,
    double* g, double* curv, double* largest)
{
    __shared__ double sh[BLOCK];
    __shared__ double wl[MAXD];
    __shared__ double coef[TILE];
    __shared__ unsigned sa[TILE];
    __shared__ unsigned sb[TILE];
    int k = blockIdx.x;
    if (k >= m) return;
    unsigned r = list[k];
    size_t base = (size_t)r * d;
    for (int j = threadIdx.x; j < d; j += BLOCK) wl[j] = w[base + j];
    __syncthreads();
    double acc[MAXD / BLOCK];
    for (int q = 0; q < MAXD / BLOCK; ++q) acc[q] = 0.0;
    unsigned n0 = off[r], n1 = off[r + 1];
    for (unsigned t0 = n0; t0 < n1; t0 += TILE) {
        unsigned cnt = min((unsigned)TILE, n1 - t0);
        for (unsigned t = threadIdx.x; t < cnt; t += BLOCK) {
            unsigned n = t0 + t;
            double z = ty[n] * (xdot(phi, d, ta[n], tb[n], wl) - tt[n]) / ttau[n];
            double s = sigmoid_(z);
            coef[t] = tom[n] * ty[n] * sigmoid_(-z) / ttau[n];
            curv[n] = sqrt(tom[n] * s * (1.0 - s)) / ttau[n];
            sa[t] = ta[n];
            sb[t] = tb[n];
        }
        __syncthreads();
        for (int q = 0; q < MAXD / BLOCK; ++q) {
            int j = threadIdx.x + q * BLOCK;
            if (j < d)
                for (unsigned t = 0; t < cnt; ++t) acc[q] += coef[t] * x_at(phi, d, sa[t], sb[t], j);
        }
        __syncthreads();
    }
    double mx = 0.0;
    for (int q = 0; q < MAXD / BLOCK; ++q) {
        int j = threadIdx.x + q * BLOCK;
        if (j < d) {
            double gj = acc[q] - (wl[j] - m0[j]) * s0inv[j];
            g[base + j] = gj;
            mx = fmax(mx, fabs(gj));
        }
    }
    mx = block_max(mx, sh);
    if (threadIdx.x == 0) largest[k] = mx;
}

// M_k (d x width): column c is sqrt(h_n) S0^1/2 x_n for the reader's c-th term, 0 past its last.
extern "C" __global__ void build(
    int m, const unsigned* list, int width, const unsigned* off, const unsigned* ta,
    const unsigned* tb, const double* curv, const float* phi, const double* s0, int d, float* M)
{
    size_t per = (size_t)width * d;
    size_t total = (size_t)m * per;
    for (size_t e = (size_t)blockIdx.x * blockDim.x + threadIdx.x; e < total;
         e += (size_t)gridDim.x * blockDim.x) {
        size_t k = e / per;
        size_t rem = e - k * per;
        unsigned col = (unsigned)(rem / d);
        int j = (int)(rem - (size_t)col * d);
        unsigned r = list[k];
        unsigned n = off[r] + col;
        float v = 0.0f;
        if (n < off[r + 1]) v = (float)(curv[n] * x_at(phi, d, ta[n], tb[n], j) * sqrt(s0[j]));
        M[e] = v;
    }
}

// H_k += (1 + jitter_k) I.
extern "C" __global__ void identity(int m, float* H, const float* jitter, int d) {
    int e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m * d) return;
    int k = e / d, j = e - (e / d) * d;
    H[(size_t)k * d * d + (size_t)j * d + j] += 1.0f + jitter[k];
}

// The right-hand side S0^1/2 g of the preconditioned Newton system, in f32.
extern "C" __global__ void rhs(int m, const unsigned* list, const double* g, const double* s0,
                               int d, float* B) {
    int e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m * d) return;
    int k = e / d, j = e - (e / d) * d;
    B[e] = (float)(g[(size_t)list[k] * d + j] * sqrt(s0[j]));
}

// step = S0^1/2 (the preconditioned solution).
extern "C" __global__ void scatter(int m, const unsigned* list, const float* B,
                                   const double* s0, int d, double* step) {
    int e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m * d) return;
    int k = e / d, j = e - (e / d) * d;
    step[(size_t)list[k] * d + j] = sqrt(s0[j]) * (double)B[e];
}

extern "C" __global__ void accept(int m, const unsigned* list, const double* w_trial,
                                  double* w, int d) {
    int e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m * d) return;
    int k = e / d, j = e - (e / d) * d;
    size_t i = (size_t)list[k] * d + j;
    w[i] = w_trial[i];
}

extern "C" __global__ void reset(int m, const unsigned* list, const double* m0, double* w,
                                 int d) {
    int e = blockIdx.x * blockDim.x + threadIdx.x;
    if (e >= m * d) return;
    int k = e / d, j = e - (e / d) * d;
    w[(size_t)list[k] * d + j] = m0[j];
}

// Each listed reader's factor H_k into its slot.
extern "C" __global__ void keep(int m, const unsigned* list, const float* H, float* factors,
                                int d) {
    size_t per = (size_t)d * d;
    size_t total = (size_t)m * per;
    for (size_t e = (size_t)blockIdx.x * blockDim.x + threadIdx.x; e < total;
         e += (size_t)gridDim.x * blockDim.x) {
        size_t k = e / per;
        factors[(size_t)list[k] * per + (e - k * per)] = H[e];
    }
}

// x^T Sigma x = |L^-1 S0^1/2 x|^2 for x = phi(a) - phi(b), with L the reader's lower factor:
// one warp per pair, forward substitution column by column.
extern "C" __global__ void quad(int np, const unsigned* pr, const unsigned* pa,
                                const unsigned* pb, const float* factors, const float* phi,
                                const double* s0, int d, double* out) {
    __shared__ double v[WARPS][MAXD];
    int warp = threadIdx.x / 32, lane = threadIdx.x % 32;
    int p = blockIdx.x * WARPS + warp;
    if (p >= np) return;
    const float* L = factors + (size_t)pr[p] * d * d;
    double* x = v[warp];
    for (int j = lane; j < d; j += 32) x[j] = x_at(phi, d, pa[p], pb[p], j) * sqrt(s0[j]);
    __syncwarp();
    double acc = 0.0;
    for (int j = 0; j < d; ++j) {
        double y = x[j] / (double)L[(size_t)j * d + j];
        acc += y * y;
        for (int i = j + 1 + lane; i < d; i += 32) x[i] -= (double)L[(size_t)j * d + i] * y;
        __syncwarp();
    }
    if (lane == 0) out[p] = acc;
}
"#;

struct Kernels {
    objective: CudaFunction,
    gradient: CudaFunction,
    build: CudaFunction,
    identity: CudaFunction,
    rhs: CudaFunction,
    scatter: CudaFunction,
    accept: CudaFunction,
    reset: CudaFunction,
    keep: CudaFunction,
    quad: CudaFunction,
}

/// The group's terms on the device, and each term's curvature from the last gradient.
struct DeviceTerms {
    off: CudaSlice<u32>,
    a: CudaSlice<u32>,
    b: CudaSlice<u32>,
    t: CudaSlice<f64>,
    y: CudaSlice<f64>,
    tau: CudaSlice<f64>,
    omega: CudaSlice<f64>,
    curvature: CudaSlice<f64>,
    /// The offsets on the host, for the Hessians' widths.
    host_off: Vec<u32>,
}

/// One group's state on the device.
struct Group {
    d: usize,
    readers: usize,
    phi: CudaSlice<f32>,
    m0: CudaSlice<f64>,
    s0: CudaSlice<f64>,
    s0inv: CudaSlice<f64>,
    w: CudaSlice<f64>,
    trial: CudaSlice<f64>,
    g: CudaSlice<f64>,
    step: CudaSlice<f64>,
    /// Per reader, the last factor `d * d` (lower triangle).
    factors: CudaSlice<f32>,
    terms: Option<DeviceTerms>,
}

/// An NVIDIA GPU as the batched path's [`Device`].
pub struct Cuda {
    stream: Arc<CudaStream>,
    blas: CudaBlas,
    solver: DnHandle,
    k: Kernels,
    group: Option<Group>,
    cands: Vec<Option<(usize, CudaSlice<f32>)>>,
}

/// A device call that cannot fail in a correct program; failing, the run cannot go on.
fn ok<T, E: std::fmt::Debug>(r: Result<T, E>, what: &str) -> T {
    r.unwrap_or_else(|e| panic!("CUDA: {what}: {e:?}"))
}

/// One thread per element, 256 per block.
fn elements(n: usize) -> LaunchConfig {
    LaunchConfig {
        grid_dim: (n.div_ceil(256).clamp(1, 65_535) as u32, 1, 1),
        block_dim: (256, 1, 1),
        shared_mem_bytes: 0,
    }
}

/// One block of [`BLOCK`] threads per listed reader.
fn per_reader(m: usize) -> LaunchConfig {
    LaunchConfig {
        grid_dim: (m as u32, 1, 1),
        block_dim: (BLOCK, 1, 1),
        shared_mem_bytes: 0,
    }
}

impl Cuda {
    /// The first visible GPU (`CUDA_VISIBLE_DEVICES`), with the kernels compiled for it.
    pub fn new() -> anyhow::Result<Cuda> {
        // SAFETY: probes whether the libraries can be loaded, without calling into them.
        let present = unsafe {
            (
                cudarc::driver::sys::is_culib_present(),
                cudarc::nvrtc::sys::is_culib_present(),
                cudarc::cublas::sys::is_culib_present(),
                cudarc::cusolver::sys::is_culib_present(),
            )
        };
        anyhow::ensure!(present.0, "no CUDA driver (libcuda) on this machine");
        anyhow::ensure!(
            present.1 && present.2 && present.3,
            "CUDA libraries missing (NVRTC {}, cuBLAS {}, cuSOLVER {}): the CUDA 13 runtime is \
             needed",
            present.1,
            present.2,
            present.3
        );
        let ctx = CudaContext::new(0).context("no usable CUDA device")?;
        // One stream only: cudarc's cross-stream event tracking is not needed.
        // SAFETY: every allocation and launch below is on `stream`.
        unsafe { ctx.disable_event_tracking() };
        let stream = ctx.default_stream();
        let ptx = compile_ptx_with_opts(
            SOURCE,
            CompileOptions {
                arch: Some("compute_75"),
                ..Default::default()
            },
        )
        .context("compiling the kernels")?;
        let module = ctx.load_module(ptx).context("loading the kernels")?;
        let f = |name: &str| module.load_function(name).context(name.to_string());
        let k = Kernels {
            objective: f("objective")?,
            gradient: f("gradient")?,
            build: f("build")?,
            identity: f("identity")?,
            rhs: f("rhs")?,
            scatter: f("scatter")?,
            accept: f("accept")?,
            reset: f("reset")?,
            keep: f("keep")?,
            quad: f("quad")?,
        };
        let blas = CudaBlas::new(stream.clone()).context("cuBLAS")?;
        let solver = DnHandle::new(stream.clone()).context("cuSOLVER")?;
        tracing::info!(
            device = ctx.name().unwrap_or_default(),
            "benchmark device ready"
        );
        Ok(Cuda {
            stream,
            blas,
            solver,
            k,
            group: None,
            cands: Vec::new(),
        })
    }

    fn group(&self) -> &Group {
        self.group.as_ref().expect("a group is loaded")
    }

    fn terms_of(&self) -> &DeviceTerms {
        self.group()
            .terms
            .as_ref()
            .expect("the group's terms are set")
    }

    fn upload<T: cudarc::driver::DeviceRepr + Clone + Default>(&self, v: &[T]) -> CudaSlice<T> {
        // Never an empty allocation.
        if v.is_empty() {
            return ok(self.stream.clone_htod(&[T::default()]), "upload");
        }
        ok(self.stream.clone_htod(v), "upload")
    }

    fn zeros<T: cudarc::driver::DeviceRepr + cudarc::driver::ValidAsZeroBits>(
        &self,
        n: usize,
    ) -> CudaSlice<T> {
        ok(self.stream.alloc_zeros::<T>(n.max(1)), "alloc")
    }

    /// Device pointers to `count` matrices of `stride` f32 each in `buf`, for the batched
    /// cuSOLVER calls.
    fn pointers(&self, buf: &CudaSlice<f32>, count: usize, stride: usize) -> CudaSlice<u64> {
        let (base, _sync) = buf.device_ptr(&self.stream);
        let p: Vec<u64> = (0..count)
            .map(|k| base + (k * stride * std::mem::size_of::<f32>()) as u64)
            .collect();
        self.upload(&p)
    }

    /// `S0^½ H S0^½ = I + M M^T` for the listed readers (at most [`SUB`]) with the curvatures of
    /// the last gradient, factored in place (lower Cholesky); a failed factor is rebuilt with a
    /// growing diagonal jitter. Returns the readers on the device, the factors and their
    /// pointers.
    fn factorize(&mut self, readers: &[u32]) -> (CudaSlice<u32>, CudaSlice<f32>, CudaSlice<u64>) {
        let g = self.group();
        let t = self.terms_of();
        let (d, m) = (g.d, readers.len());
        let width = readers
            .iter()
            .map(|&r| (t.host_off[r as usize + 1] - t.host_off[r as usize]) as usize)
            .max()
            .unwrap_or(0)
            .max(1);
        let list = self.upload(readers);
        let mut big = self.zeros::<f32>(m * d * width);
        let (mi, wi, di) = (m as i32, width as i32, d as i32);
        let mut b = self.stream.launch_builder(&self.k.build);
        b.arg(&mi)
            .arg(&list)
            .arg(&wi)
            .arg(&t.off)
            .arg(&t.a)
            .arg(&t.b)
            .arg(&t.curvature)
            .arg(&g.phi)
            .arg(&g.s0)
            .arg(&di)
            .arg(&mut big);
        // SAFETY: the kernel writes m * width * d elements of `big`, reads within the terms.
        ok(unsafe { b.launch(elements(m * d * width)) }, "build");
        let mut h = self.zeros::<f32>(m * d * d);
        let pointers = self.pointers(&h, m, d * d);
        let mut jitter = vec![0.0f32; m];
        for attempt in 0..=JITTER_TRIES {
            let cfg = StridedBatchedConfig {
                gemm: GemmConfig {
                    transa: cublasOperation_t::CUBLAS_OP_N,
                    transb: cublasOperation_t::CUBLAS_OP_T,
                    m: di,
                    n: di,
                    k: wi,
                    alpha: 1.0f32,
                    lda: di,
                    ldb: di,
                    beta: 0.0f32,
                    ldc: di,
                },
                batch_size: mi,
                stride_a: (d * width) as i64,
                stride_b: (d * width) as i64,
                stride_c: (d * d) as i64,
            };
            // SAFETY: m matrices of d x width in `big`, m of d x d in `h`.
            ok(
                unsafe { self.blas.gemm_strided_batched(cfg, &big, &big, &mut h) },
                "Hessian SGEMM",
            );
            let jit = self.upload(&jitter);
            let mut b = self.stream.launch_builder(&self.k.identity);
            b.arg(&mi).arg(&mut h).arg(&jit).arg(&di);
            // SAFETY: adds to the diagonals of the m matrices in `h`.
            ok(unsafe { b.launch(elements(m * d)) }, "identity");
            let mut info = self.zeros::<i32>(m);
            {
                let (hp, _s1) = pointers.device_ptr(&self.stream);
                let (ip, _s2) = info.device_ptr_mut(&self.stream);
                // SAFETY: m pointers to d x d matrices with leading dimension d; m info slots.
                ok(
                    unsafe {
                        solver::cusolverDnSpotrfBatched(
                            self.solver.cu(),
                            solver::cublasFillMode_t::CUBLAS_FILL_MODE_LOWER,
                            di,
                            hp as *mut *mut f32,
                            di,
                            ip as *mut i32,
                            mi,
                        )
                    }
                    .result(),
                    "potrfBatched",
                );
            }
            let info = ok(self.stream.clone_dtoh(&info), "info");
            let failed: Vec<usize> = (0..m).filter(|&k| info[k] != 0).collect();
            if failed.is_empty() {
                return (list, h, pointers);
            }
            assert!(
                attempt < JITTER_TRIES,
                "no f32 Cholesky factor for {} readers after jitter",
                failed.len()
            );
            for k in failed {
                jitter[k] = if jitter[k] == 0.0 {
                    JITTER
                } else {
                    jitter[k] * 100.0
                };
            }
        }
        unreachable!("returns or panics in the loop")
    }
}

impl Device for Cuda {
    fn load(&mut self, d: usize, phi: &[f32], m0: &[f64], s0: &[f64], readers: usize) {
        assert!(
            d <= MAXD,
            "vectors of {d} numbers: the kernels hold at most {MAXD}"
        );
        let s0inv: Vec<f64> = s0.iter().map(|v| 1.0 / v).collect();
        let w: Vec<f64> = m0.repeat(readers.max(1));
        let group = Group {
            d,
            readers,
            phi: self.upload(phi),
            m0: self.upload(m0),
            s0: self.upload(s0),
            s0inv: self.upload(&s0inv),
            w: self.upload(&w),
            trial: self.zeros(readers * d),
            g: self.zeros(readers * d),
            step: self.zeros(readers * d),
            factors: self.zeros(readers * d * d),
            terms: None,
        };
        // The last group's memory goes first.
        self.group = None;
        self.group = Some(group);
    }

    fn terms(&mut self, terms: &Terms) {
        let readers = self.group().readers;
        assert_eq!(terms.off.len(), readers + 1, "terms for every reader");
        let t = DeviceTerms {
            off: self.upload(&terms.off),
            a: self.upload(&terms.a),
            b: self.upload(&terms.b),
            t: self.upload(&terms.t),
            y: self.upload(&terms.y),
            tau: self.upload(&terms.tau),
            omega: self.upload(&terms.omega),
            curvature: self.zeros(terms.a.len()),
            host_off: terms.off.clone(),
        };
        let g = self.group.as_mut().expect("a group is loaded");
        g.terms = None;
        g.terms = Some(t);
    }

    fn reset(&mut self, readers: &[u32]) {
        if readers.is_empty() {
            return;
        }
        let list = self.upload(readers);
        let g = self.group.as_mut().expect("a group is loaded");
        let (m, d) = (readers.len() as i32, g.d as i32);
        let mut b = self.stream.launch_builder(&self.k.reset);
        b.arg(&m).arg(&list).arg(&g.m0).arg(&mut g.w).arg(&d);
        // SAFETY: writes the listed readers' rows of w.
        ok(unsafe { b.launch(elements(readers.len() * g.d)) }, "reset");
    }

    fn objective(&mut self, readers: &[u32], alpha: Option<&[f64]>) -> Vec<f64> {
        if readers.is_empty() {
            return Vec::new();
        }
        let list = self.upload(readers);
        let alpha_dev = self.upload(alpha.unwrap_or(&[]));
        let mut out = self.zeros::<f64>(readers.len());
        let g = self.group.as_mut().expect("a group is loaded");
        let t = g.terms.as_ref().expect("the group's terms are set");
        let (m, d, trial) = (readers.len() as i32, g.d as i32, i32::from(alpha.is_some()));
        let mut b = self.stream.launch_builder(&self.k.objective);
        b.arg(&m)
            .arg(&list)
            .arg(&g.w)
            .arg(&g.step)
            .arg(&alpha_dev)
            .arg(&trial)
            .arg(&mut g.trial)
            .arg(&t.off)
            .arg(&t.a)
            .arg(&t.b)
            .arg(&t.t)
            .arg(&t.y)
            .arg(&t.tau)
            .arg(&t.omega)
            .arg(&g.phi)
            .arg(&g.m0)
            .arg(&g.s0inv)
            .arg(&d)
            .arg(&mut out);
        // SAFETY: one block per listed reader, within its rows and terms.
        ok(unsafe { b.launch(per_reader(readers.len())) }, "objective");
        ok(self.stream.clone_dtoh(&out), "objective")
    }

    fn accept(&mut self, readers: &[u32]) {
        if readers.is_empty() {
            return;
        }
        let list = self.upload(readers);
        let g = self.group.as_mut().expect("a group is loaded");
        let (m, d) = (readers.len() as i32, g.d as i32);
        let mut b = self.stream.launch_builder(&self.k.accept);
        b.arg(&m).arg(&list).arg(&g.trial).arg(&mut g.w).arg(&d);
        // SAFETY: copies the listed readers' rows.
        ok(unsafe { b.launch(elements(readers.len() * g.d)) }, "accept");
    }

    fn gradient(&mut self, readers: &[u32]) -> Vec<f64> {
        if readers.is_empty() {
            return Vec::new();
        }
        let list = self.upload(readers);
        let mut largest = self.zeros::<f64>(readers.len());
        let g = self.group.as_mut().expect("a group is loaded");
        let t = g.terms.as_mut().expect("the group's terms are set");
        let (m, d) = (readers.len() as i32, g.d as i32);
        let mut b = self.stream.launch_builder(&self.k.gradient);
        b.arg(&m)
            .arg(&list)
            .arg(&g.w)
            .arg(&t.off)
            .arg(&t.a)
            .arg(&t.b)
            .arg(&t.t)
            .arg(&t.y)
            .arg(&t.tau)
            .arg(&t.omega)
            .arg(&g.phi)
            .arg(&g.m0)
            .arg(&g.s0inv)
            .arg(&d)
            .arg(&mut g.g)
            .arg(&mut t.curvature)
            .arg(&mut largest);
        // SAFETY: one block per listed reader, within its rows and terms.
        ok(unsafe { b.launch(per_reader(readers.len())) }, "gradient");
        ok(self.stream.clone_dtoh(&largest), "gradient")
    }

    fn step(&mut self, readers: &[u32]) {
        for part in readers.chunks(SUB) {
            let (list, h, pointers) = self.factorize(part);
            let g = self.group();
            let (m, d) = (part.len() as i32, g.d as i32);
            let mut rhs = self.zeros::<f32>(part.len() * g.d);
            let mut b = self.stream.launch_builder(&self.k.rhs);
            b.arg(&m)
                .arg(&list)
                .arg(&g.g)
                .arg(&g.s0)
                .arg(&d)
                .arg(&mut rhs);
            // SAFETY: m * d elements of `rhs`.
            ok(unsafe { b.launch(elements(part.len() * g.d)) }, "rhs");
            let rhs_pointers = self.pointers(&rhs, part.len(), g.d);
            let mut info = self.zeros::<i32>(1);
            {
                let (hp, _s1) = pointers.device_ptr(&self.stream);
                let (bp, _s2) = rhs_pointers.device_ptr(&self.stream);
                let (ip, _s3) = info.device_ptr_mut(&self.stream);
                // SAFETY: m factors d x d and m right-hand sides of d, leading dimension d.
                ok(
                    unsafe {
                        solver::cusolverDnSpotrsBatched(
                            self.solver.cu(),
                            solver::cublasFillMode_t::CUBLAS_FILL_MODE_LOWER,
                            d,
                            1,
                            hp as *mut *mut f32,
                            d,
                            bp as *mut *mut f32,
                            d,
                            ip as *mut i32,
                            m,
                        )
                    }
                    .result(),
                    "potrsBatched",
                );
            }
            drop(h);
            let g = self.group.as_mut().expect("a group is loaded");
            let mut b = self.stream.launch_builder(&self.k.scatter);
            b.arg(&m)
                .arg(&list)
                .arg(&rhs)
                .arg(&g.s0)
                .arg(&d)
                .arg(&mut g.step);
            // SAFETY: writes the listed readers' rows of step.
            ok(unsafe { b.launch(elements(part.len() * g.d)) }, "scatter");
        }
    }

    fn factor(&mut self, readers: &[u32]) {
        for part in readers.chunks(SUB) {
            let (list, h, _) = self.factorize(part);
            let g = self.group.as_mut().expect("a group is loaded");
            let (m, d) = (part.len() as i32, g.d as i32);
            let mut b = self.stream.launch_builder(&self.k.keep);
            b.arg(&m).arg(&list).arg(&h).arg(&mut g.factors).arg(&d);
            // SAFETY: copies m factors into the listed readers' slots.
            ok(
                unsafe { b.launch(elements(part.len() * g.d * g.d)) },
                "keep",
            );
        }
    }

    fn weights(&mut self, readers: &[u32]) -> Vec<f64> {
        let g = self.group();
        let w = ok(self.stream.clone_dtoh(&g.w), "weights");
        readers
            .iter()
            .flat_map(|&r| w[r as usize * g.d..(r as usize + 1) * g.d].to_vec())
            .collect()
    }

    fn quad(&mut self, pairs: &[(u32, u32, u32)]) -> Vec<f64> {
        if pairs.is_empty() {
            return Vec::new();
        }
        let pr = self.upload(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
        let pa = self.upload(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
        let pb = self.upload(&pairs.iter().map(|p| p.2).collect::<Vec<_>>());
        let mut out = self.zeros::<f64>(pairs.len());
        let g = self.group();
        let (np, d) = (pairs.len() as i32, g.d as i32);
        let mut b = self.stream.launch_builder(&self.k.quad);
        b.arg(&np)
            .arg(&pr)
            .arg(&pa)
            .arg(&pb)
            .arg(&g.factors)
            .arg(&g.phi)
            .arg(&g.s0)
            .arg(&d)
            .arg(&mut out);
        let cfg = LaunchConfig {
            grid_dim: ((pairs.len() as u32).div_ceil(WARPS), 1, 1),
            block_dim: (WARPS * 32, 1, 1),
            shared_mem_bytes: 0,
        };
        // SAFETY: one warp per pair, reading its reader's factor and two rows.
        ok(unsafe { b.launch(cfg) }, "quad");
        ok(self.stream.clone_dtoh(&out), "quad")
    }

    fn candidates(&mut self, d: usize, cand_phi: &[f32]) -> usize {
        let buf = self.upload(cand_phi);
        self.cands.push(Some((d, buf)));
        self.cands.len() - 1
    }

    fn scores(&mut self, handle: usize, w: &[f32]) -> Vec<f32> {
        let (d, cand) = self.cands[handle].as_ref().expect("a live handle");
        let (d, c) = (*d, cand.len() / d);
        let rows = w.len() / d;
        let mut out = vec![0.0f32; rows * c];
        for (part, dst) in w.chunks(SCORE_ROWS * d).zip(out.chunks_mut(SCORE_ROWS * c)) {
            let n = part.len() / d;
            let wd = self.upload(part);
            let mut mu = self.zeros::<f32>(n * c);
            // mu (c x n, column-major: row after row of candidates) = cand^T (c x d) w (d x n).
            let cfg = GemmConfig {
                transa: cublasOperation_t::CUBLAS_OP_T,
                transb: cublasOperation_t::CUBLAS_OP_N,
                m: c as i32,
                n: n as i32,
                k: d as i32,
                alpha: 1.0f32,
                lda: d as i32,
                ldb: d as i32,
                beta: 0.0f32,
                ldc: c as i32,
            };
            // SAFETY: cand holds c vectors of d, wd n of d, mu c * n.
            ok(
                unsafe { self.blas.gemm(cfg, cand, &wd, &mut mu) },
                "score SGEMM",
            );
            ok(self.stream.memcpy_dtoh(&mu, dst), "scores");
        }
        out
    }

    fn release(&mut self, handle: usize) {
        self.cands[handle] = None;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// Prints past the test harness's capture: a skipped GPU test must say so.
    fn skipped(test: &str, why: &str) {
        let _ = writeln!(std::io::stderr(), "SKIPPED {test}: {why}");
    }

    /// The kernels compile (NVRTC needs no GPU, only its library).
    #[test]
    fn kernels_compile() {
        // SAFETY: probes whether the library can be loaded.
        if !unsafe { cudarc::nvrtc::sys::is_culib_present() } {
            skipped(
                "kernels_compile",
                "no NVRTC library (libnvrtc) on this machine",
            );
            return;
        }
        compile_ptx_with_opts(
            SOURCE,
            CompileOptions {
                arch: Some("compute_75"),
                ..Default::default()
            },
        )
        .expect("the kernels compile");
    }

    /// The GPU against taste on the CPU at the product's vector size (d = 152): see
    /// `batch::tests::parity`.
    #[test]
    fn cuda_matches_taste() {
        let device = match Cuda::new() {
            Ok(d) => d,
            Err(e) => return skipped("cuda_matches_taste", &format!("{e:#}")),
        };
        let layout = taste::Layout {
            k: 64,
            r: 32,
            a: 50,
        };
        super::super::batch::tests::parity(Box::new(device), layout, 300, 1e-6);
    }
}
