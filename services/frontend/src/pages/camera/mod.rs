use gloo::utils::window;
use web_sys::{
    wasm_bindgen::{Clamped, JsCast, JsValue},
    CanvasRenderingContext2d, HtmlVideoElement, MediaStreamConstraints,
};

#[derive(Debug, Clone)]
pub struct Canvas {
    pub context: CanvasRenderingContext2d,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug)]
pub struct WebCam {
    pub video: HtmlVideoElement,
}

impl WebCam {
    pub fn new() -> Self {
        let document = window().document().unwrap();

        let video = document
            .create_element("video")
            .unwrap()
            .dyn_into::<HtmlVideoElement>()
            .unwrap();

        video.set_autoplay(true);

        Self { video }
    }

    pub async fn setup(&self) -> Result<(), JsValue> {
        let mut constraints = MediaStreamConstraints::new();
        constraints.video(&JsValue::from(true));

        let promise = window()
            .navigator()
            .media_devices()
            .unwrap()
            .get_user_media_with_constraints(&constraints)
            .unwrap();

        let stream = wasm_bindgen_futures::JsFuture::from(promise).await?;

        self.video.set_src_object(Some(&stream.into()));

        Ok(())
    }
}

impl Canvas {
    pub fn new() -> Self {
        let window = window();
        let width = window
            .inner_width()
            .unwrap()
            .as_f64()
            .expect("Could not convert to f64")
            * 0.66666;

        let height = window
            .inner_height()
            .unwrap()
            .as_f64()
            .expect("Could not convert to f64")
            * 0.66666;

        let Some(document) = window.document() else {
            tracing::error!("Failed to get document");
            panic!("Failed to get document");
        };

        let context_attributes = web_sys::ContextAttributes2d::new();
        context_attributes.set_will_read_frequently(true);

        let canvas = document
            .get_element_by_id("pre")
            .unwrap()
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .unwrap();

        canvas.set_width(width.floor() as u32);
        canvas.set_height(height.floor() as u32);

        let context = canvas
            .get_context_with_context_options("2d", &context_attributes)
            .unwrap()
            .unwrap()
            .dyn_into::<CanvasRenderingContext2d>()
            .unwrap();

        Self {
            context,
            width,
            height,
        }
    }

    pub fn draw_image(self: &Self, video: &HtmlVideoElement) {
        self.context
            .draw_image_with_html_video_element_and_dw_and_dh(
                video,
                0.,
                0.,
                self.width,
                self.height,
            )
            .unwrap();
    }

    pub fn get_image_data(self: &Self) -> Clamped<Vec<u8>> {
        self.context
            .get_image_data(0., 0., self.width, self.height)
            .unwrap()
            .data()
    }
}
