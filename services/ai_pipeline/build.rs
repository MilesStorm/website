use burn_onnx::ModelGen;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    ModelGen::new()
        .input("src/model/yolo26x.onnx")
        .out_dir("model/")
        .development(true)
        .run_from_script();
}
