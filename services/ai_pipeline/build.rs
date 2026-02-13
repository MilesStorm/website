use burn_import::onnx::ModelGen;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    ModelGen::new()
        .input("src/model/yolo26n.onnx")
        .out_dir("model/")
        .development(true)
        .run_from_script();
}
