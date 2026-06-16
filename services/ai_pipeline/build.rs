use std::path::PathBuf;

use burn_onnx::ModelGen;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/model/yolo26s.onnx");
    ModelGen::new()
        .input("src/model/yolo26s.onnx")
        .out_dir("model/")
        .run_from_script();

    patch_int64_remainder();
}

/// Work around a CubeCL/CUDA codegen bug in burn 0.21: the YOLO end-to-end head
/// recovers the class index with an int64 modulo (`topk_idx % num_classes`), which
/// CubeCL lowers to `floor(long long)` — a call CUDA rejects (`floor` only has
/// float/double overloads), crashing the very first forward pass.
///
/// burn-onnx emits this as a single `Tensor::remainder` on integer tensors in the
/// generated `Model::forward`. We rewrite that one expression to compute the
/// remainder in f32 (`a - b*floor(a/b)`) and cast back to i64. Indices here are at
/// most a few thousand, well within f32's exact-integer range, so the result is
/// identical — but CubeCL now emits the valid `floor(float)`.
fn patch_int64_remainder() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let gen_path = PathBuf::from(out_dir).join("model/yolo26s.rs");
    let src = std::fs::read_to_string(&gen_path).expect("generated model file");

    let needle = "__lhs.expand(__shape).remainder(__rhs.expand(__shape))";
    let replacement = "{ let __l = __lhs.expand(__shape).float(); \
        let __r = __rhs.expand(__shape).float(); \
        let __q = (__l.clone() / __r.clone()).floor(); \
        (__l - __r * __q).int() }";

    let count = src.matches(needle).count();
    assert_eq!(
        count, 1,
        "expected exactly one int64 remainder to patch in {gen_path:?}, found {count}; \
         the generated decode changed — re-check the CubeCL modulo workaround"
    );

    std::fs::write(&gen_path, src.replace(needle, replacement)).expect("write patched model");
}
