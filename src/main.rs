use iced::Application as _;

mod styling;
mod worker;

#[cfg(windows)]
const FONT_BYTES_REGULAR: &[u8] = include_bytes!("..\\resources\\Roboto\\Roboto-Regular.ttf");
#[cfg(unix)]
const FONT_BYTES_REGULAR: &[u8] = include_bytes!("../resources/Roboto/Roboto-Regular.ttf");
#[cfg(windows)]
const FONT_BYTES_BOLD: &[u8] = include_bytes!("..\\resources\\Roboto\\Roboto-Bold.ttf");
#[cfg(unix)]
const FONT_BYTES_BOLD: &[u8] = include_bytes!("../resources/Roboto/Roboto-Bold.ttf");

const FONT_NAME: &'static str = "Roboto";
const TOOLBAR_FONT_SIZE: u16 = 14;
const CONTENT_FONT_SIZE: u16 = 16;
const FOOTER_FONT_SIZE: u16 = 12;
const LOADING_INDICATOR_SIZE: f32 = 120.0;
const LOADING_INDICATOR_SPEED_MS: u64 = 100;

const ALLOWED_VIDEO_TYPES: [&'static str; 11] = [
    "mp4", "mov", "wmv", "avi", "avchd", "flv", "f4v", "swf", "mkv", "webm", "html5",
];

const LOG_TARGET: &'static str = "iced_gif_maker::main";

const TITLE: &'static str = "Iced Animated GIF Maker";

const SPACING_SMALL: u16 = 5;
const SPACING_NORMAL: u16 = 10;
const SPACING_LARGE: u16 = 20;

const DEFAULT_GIF_WIDTH: u16 = 480;

fn main() -> iced::Result {
    #[cfg(feature = "logging")]
    {
        if std::env::var("RUST_LOG").ok().is_none() {
            // std::env::set_var("RUST_LOG", "iced_gif_maker=debug");
            std::env::set_var("RUST_LOG", "iced_gif_maker=debug,ffmpeg_gif_maker=info");
        }
        env_logger::init();
    }

    MyApp::run(iced::Settings {
        window: iced::window::Settings {
            size: (700, 500),
            min_size: Some((400, 285)),
            position: iced::window::Position::Specific(100, 800),
            ..Default::default()
        },
        ..Default::default()
    })
}

#[derive(Debug)]
struct MyApp {
    loaded_resources_count: usize,
    error_message: Option<String>,
    progress: Option<f64>,
    video_duration: Option<std::time::Duration>,
    image_data: Option<Vec<u8>>,
    tx: Option<iced::futures::channel::mpsc::Sender<worker::Input>>,
    frames: Option<iced_gif::gif::Frames>,
    video_path: Option<std::path::PathBuf>,
    gif_width: Option<u16>,
    idle: bool,
}

#[derive(Debug, Clone)]
enum MyMessage {
    FontLoaded,
    ConvertMessageSentToWorker,
    CancelMessageSentToWorker,
    WorkerEvent(worker::Event),
    GifFramesLoaded(Result<iced_gif::gif::Frames, iced_gif::gif::Error>),
    Event(iced::Event),
    Clear,
    SelectFile,
    FileSelected(Option<std::path::PathBuf>),
    Width(Option<u16>),
    SaveResult(Result<bool, String>),
    Save,
}

impl Default for MyApp {
    fn default() -> Self {
        Self {
            loaded_resources_count: 0,
            progress: None,
            image_data: None,
            video_duration: None,
            tx: None,
            frames: None,
            video_path: None,
            gif_width: Some(DEFAULT_GIF_WIDTH),
            error_message: None,
            idle: true,
        }
    }
}

impl MyApp {
    fn font(&self) -> iced::Font {
        iced::Font {
            weight: iced::font::Weight::Normal,
            family: iced::font::Family::Name(FONT_NAME),
            monospaced: true,
            stretch: iced::font::Stretch::Normal,
        }
    }

    fn bold_font(&self) -> iced::Font {
        iced::Font {
            weight: iced::font::Weight::Bold,
            ..self.font()
        }
    }

    fn are_resources_loaded(&self) -> bool {
        self.loaded_resources_count == 2
    }

    fn clear_all(&mut self) {
        self.frames = None;
        self.image_data = None;
        self.progress = None;
        self.video_duration = None;
        self.video_path = None;
        self.error_message = None;
        self.idle = true;
    }

    fn select_file(&mut self) -> iced::Command<MyMessage> {
        log::debug!(target: LOG_TARGET, "Presenting video file picker...");
        iced::Command::perform(
            async {
                let file = rfd::AsyncFileDialog::new()
                    .add_filter("video", &ALLOWED_VIDEO_TYPES)
                    .pick_file()
                    .await;
                file.map(|handle| handle.path().to_path_buf())
            },
            MyMessage::FileSelected,
        )
    }

    fn new_task(&mut self, path: std::path::PathBuf) -> iced::Command<MyMessage> {
        log::debug!(target: LOG_TARGET, "New task requested...");

        use iced::futures::sink::SinkExt;

        let Some(tx) = self.tx.as_ref() else {
            log::debug!(target: LOG_TARGET, "Task ignored because worker not ready.");
            return iced::Command::none();
        };
        let mut tx = tx.clone();

        if !self.idle {
            log::debug!(target: LOG_TARGET, "Task ignored because one is already ongoing.");
            return iced::Command::none();
        }

        self.clear_all();

        self.idle = false;
        self.video_path = Some(path.clone());

        let settings = {
            let settings = ffmpeg_gif_maker::Settings::with_standard_fps(
                path.to_string_lossy().to_string(),
                self.gif_width.unwrap_or(DEFAULT_GIF_WIDTH),
            );
            if let Some(ffmpeg_path) = std::env::var("ICED_GIF_MAKER_FFMPEG_PATH").ok() {
                log::debug!(target: LOG_TARGET, "Custom ffmpeg binary path provided through ICED_GIF_MAKER_FFMPEG_PATH environment variable: {}", ffmpeg_path);
                settings.ffmpeg_path(ffmpeg_path)
            } else {
                settings
            }
        };

        log::debug!(target: LOG_TARGET, "Sending new task to worker...");
        iced::Command::perform(
            async move { tx.send(worker::Input::NewTask(settings)).await },
            |_| MyMessage::ConvertMessageSentToWorker,
        )
    }

    fn is_working(&self) -> bool {
        self.video_path.is_some() && self.frames.is_none() && self.error_message.is_none()
    }

    fn is_cleared(&self) -> bool {
        self.video_path.is_none() && self.idle
    }

    fn save_to_file(&self) -> iced::Command<MyMessage> {
        log::debug!(target: LOG_TARGET, "Presenting video file picker (for saving)...");

        let (Some(path), Some(data)) = (&self.video_path, &self.image_data) else {
            log::error!(target: LOG_TARGET, "This method should not get called while there is no image data.");
            panic!();
        };

        let mut path = path.clone();
        path.set_extension("gif");

        let data = data.clone();
        let f = async move {
            let file_name = path
                .file_name()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or("unnamed.gif".into());
            let file_directory = path
                .parent()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or("".into());

            let result = rfd::AsyncFileDialog::new()
                .set_file_name(file_name)
                .set_directory(file_directory)
                .save_file()
                .await;

            let Some(handle) = result else {
                return Ok(false);
            };

            tokio::fs::write(handle.path(), data)
                .await
                .map(|_| true)
                .map_err(|e| e.to_string())
        };

        log::debug!(target: LOG_TARGET, "Dispatching 'save' command...");
        iced::Command::perform(f, MyMessage::SaveResult)
    }

    fn view_footer(&self) -> iced::Element<'_, MyMessage> {
        let status_message = if let (Some(_), Some(video_path)) =
            (self.error_message.as_ref(), self.video_path.as_ref())
        {
            format!("Failed to convert file: {:?}", video_path)
        } else if self.frames.is_some() {
            "Previewing animated GIF".into()
        } else if self.image_data.is_some() {
            "Conversion successful! Loading animated GIF...".into()
        } else if let Some(path) = self.video_path.as_ref() {
            format!("Video path: {:?}", path)
        } else {
            "".into()
        };

        let text = iced::widget::text(status_message)
            .font(self.font())
            .size(FOOTER_FONT_SIZE);

        let mut row = iced::widget::Row::new()
            .width(iced::Length::Fill)
            .spacing(SPACING_SMALL)
            .align_items(iced::Alignment::Center);

        if self.image_data.is_some() && self.frames.is_none() && self.error_message.is_none() {
            let loading_indicator =
                iced_loading_indicator::Widget::new(FOOTER_FONT_SIZE as f32, None, true)
                    .tick_duration_ms(LOADING_INDICATOR_SPEED_MS);
            row = row.push(loading_indicator)
        }

        row = row.push(text);

        iced::widget::container(row)
            .padding([SPACING_SMALL, SPACING_LARGE])
            .style(styling::CustomContainer::default().move_to_style())
            .height(iced::Length::Shrink)
            .width(iced::Length::Fill)
            .into()
    }

    fn view_toolbar(&self) -> iced::Element<'_, MyMessage> {
        let mut row = iced::widget::Row::new();

        if self.is_cleared() {
            // NOTE: Could be "!is_working()", but three buttons looks too
            // busy, so user will have to first clear and then "open" again.
            let text = iced::widget::text("Open".to_uppercase())
                .font(self.bold_font())
                .size(TOOLBAR_FONT_SIZE);
            let button = iced::widget::button(text)
                .on_press(MyMessage::SelectFile)
                .style(styling::ToolbarButton::default().into());
            row = row.push(button);
        }

        if !self.is_cleared() {
            let text = if self.is_working() { "Cancel" } else { "Clear" }.to_uppercase();
            let text = iced::widget::text(text)
                .font(self.bold_font())
                .size(TOOLBAR_FONT_SIZE);

            let button = iced::widget::button(text)
                .on_press(MyMessage::Clear)
                .style(styling::ToolbarButton::destructive().into());
            row = row.push(button);
        }

        if self.image_data.is_some() {
            let text = iced::widget::text("Save".to_uppercase())
                .font(self.bold_font())
                .size(TOOLBAR_FONT_SIZE);
            let button = iced::widget::button(text)
                .on_press(MyMessage::Save)
                .style(styling::ToolbarButton::default().into());
            row = row.push(button);
        }

        row = row.push(iced::widget::horizontal_space(iced::Length::Fill));

        let input_width = {
            let input = numeric_input::NumericInput::new(self.gif_width, MyMessage::Width)
                .placeholder(format!("{}", DEFAULT_GIF_WIDTH))
                .size(TOOLBAR_FONT_SIZE)
                .font(self.font())
                .disabled(self.is_working());

            let label = iced::widget::text("Width (px): ")
                .font(self.bold_font())
                .size(TOOLBAR_FONT_SIZE);

            iced::widget::row!(label, input)
                .width(iced::Length::Shrink)
                .spacing(0)
                .align_items(iced::Alignment::Center)
        };
        row = row.push(input_width);

        row = row
            .width(iced::Length::Fill)
            .align_items(iced::Alignment::Center)
            .spacing(SPACING_NORMAL)
            .height(iced::Length::Shrink);

        iced::widget::container(row)
            .width(iced::Length::Fill)
            .height(iced::Length::Shrink)
            .style(styling::CustomContainer::toolbar().move_to_style())
            .padding([SPACING_NORMAL + SPACING_SMALL, SPACING_LARGE])
            .into()
    }

    fn view_content(&self) -> iced::Element<'_, MyMessage> {
        let element: iced::Element<'_, MyMessage> = if let Some(error_message) =
            self.error_message.as_ref()
        {
            let text = iced::widget::text(format!("[ERROR] {}", error_message))
                .font(self.font())
                .size(CONTENT_FONT_SIZE);
            iced::widget::container(text).into()
        } else if let Some(frames) = self.frames.as_ref() {
            let image = iced_gif::gif(frames).content_fit(iced::ContentFit::ScaleDown);
            image.into()
        } else if let Some(data) = &self.image_data {
            let image =
                iced::widget::Image::new(iced::widget::image::Handle::from_memory(data.clone()))
                    .content_fit(iced::ContentFit::ScaleDown);
            image.into()
        } else if self.is_working() {
            let message = if let (Some(_), Some(progress)) =
                (self.video_duration.as_ref(), self.progress.as_ref())
            {
                format!("Processing frames - {:.0}%", progress * 100.0)
            } else if let Some(video_duration) = self.video_duration.as_ref() {
                format!(
                    "Video duration parsed ({:?}). Waiting for frame processing to start...",
                    video_duration
                )
            } else {
                "Creating FFmpeg task...".into()
            };

            let text = iced::widget::text(message)
                .font(self.font())
                .size(CONTENT_FONT_SIZE);

            let loading_indicator =
                iced_loading_indicator::Widget::new(LOADING_INDICATOR_SIZE, None, true)
                    .tick_duration_ms(LOADING_INDICATOR_SPEED_MS);

            iced::widget::container(
                iced::widget::column!(loading_indicator, text)
                    .align_items(iced::Alignment::Center)
                    .spacing(SPACING_LARGE)
                    .width(iced::Length::Fill),
            )
            .padding(0)
            .center_x()
            .width(iced::Length::Fill)
            .into()
        } else {
            iced::widget::text("Select a video file or drag-and-drop one here")
                .font(self.font())
                .size(CONTENT_FONT_SIZE)
                .into()
        };

        iced::widget::container(element)
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .padding([SPACING_NORMAL, SPACING_LARGE])
            .center_x()
            .center_y()
            .into()
    }

    fn view_full(&self) -> iced::Element<'_, MyMessage> {
        let toolbar = self.view_toolbar();
        let footer = self.view_footer();
        let divider_toolbar =
            iced::widget::horizontal_rule(0).style(styling::CustomRule::dark().move_to_style());

        let content = self.view_content();

        let column = iced::widget::column!(toolbar, divider_toolbar, content, footer)
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .align_items(iced::Alignment::Center)
            .spacing(0);

        iced::widget::container(column)
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .into()
    }
}

impl iced::Application for MyApp {
    type Executor = iced::executor::Default;
    type Flags = ();
    type Message = MyMessage;
    type Theme = iced::theme::Theme;

    fn subscription(&self) -> iced::Subscription<Self::Message> {
        iced::Subscription::batch(vec![
            iced::subscription::events().map(MyMessage::Event),
            worker::worker().map(MyMessage::WorkerEvent),
        ])
    }

    fn new(_flags: Self::Flags) -> (Self, iced::Command<Self::Message>) {
        let commands: Vec<iced::Command<MyMessage>> = vec![FONT_BYTES_REGULAR, FONT_BYTES_BOLD]
            .iter()
            .map(|&bytes| {
                iced::font::load(std::borrow::Cow::from(bytes)).map(|r| {
                    if let Err(e) = r {
                        panic!("{:?}", e);
                    }
                    MyMessage::FontLoaded
                })
            })
            .collect();

        (Default::default(), iced::Command::batch(commands))
    }

    fn title(&self) -> String {
        TITLE.into()
    }

    fn theme(&self) -> Self::Theme {
        styling::CustomTheme::new().to_theme()
    }

    fn view(&self) -> iced::Element<'_, Self::Message, iced::Renderer<Self::Theme>> {
        if !self.are_resources_loaded() {
            return iced::widget::container("")
                .width(iced::Length::Fill)
                .height(iced::Length::Fill)
                .center_x()
                .center_y()
                .padding(SPACING_LARGE)
                .into();
        }

        self.view_full()
    }

    fn update(&mut self, message: Self::Message) -> iced::Command<Self::Message> {
        match message {
            MyMessage::FontLoaded => {
                self.loaded_resources_count += 1;
                log::debug!(target: LOG_TARGET, "Font loaded message received. Current count: {}", self.loaded_resources_count);
                iced::Command::none()
            }
            MyMessage::Save => {
                log::debug!(target: LOG_TARGET, "Save message received.");
                self.save_to_file()
            }
            MyMessage::SaveResult(result) => match result {
                Ok(saved) => {
                    // NOTE: False here simply means that the operation was cancelled.
                    log::info!(target: LOG_TARGET, "File saved: {}", saved);
                    iced::Command::none()
                }
                Err(e) => {
                    // Should warn user about failure to save...
                    log::warn!(target: LOG_TARGET, "Failed to save file: {:?}", e);
                    iced::Command::none()
                }
            },
            MyMessage::Width(width) => {
                self.gif_width = width;
                log::debug!(target: LOG_TARGET, "Gif width changed: {:?}", width);
                iced::Command::none()
            }
            MyMessage::ConvertMessageSentToWorker => {
                log::debug!(target: LOG_TARGET, "Conversion task sent to worker.");
                iced::Command::none()
            }
            MyMessage::Clear => {
                if self.idle {
                    log::info!(target: LOG_TARGET, "Clear button: no action.");
                    iced::Command::perform(async {}, |_| MyMessage::CancelMessageSentToWorker)
                } else if let Some(tx) = self.tx.as_ref() {
                    log::info!(target: LOG_TARGET, "Clear button: some action.");
                    use iced::futures::sink::SinkExt;
                    let mut tx = tx.clone();
                    let f = async move {
                        let input = worker::Input::Command(ffmpeg_gif_maker::Command::Cancel);
                        tx.send(input).await
                    };
                    // Should I be ignoring `send` errors here?
                    log::debug!(target: LOG_TARGET, "Dispatching command to send cancellation request to worker...");
                    iced::Command::perform(f, |_| MyMessage::CancelMessageSentToWorker)
                } else {
                    log::debug!(target: LOG_TARGET, "Nothing to clear.");
                    iced::Command::none()
                }
            }
            MyMessage::SelectFile => {
                log::debug!(target: LOG_TARGET, "Received message requesting file selection. Calling command generator method...");
                self.select_file()
            }
            MyMessage::FileSelected(path) => {
                log::info!(target: LOG_TARGET, "File selected: {:?}", path);
                if let Some(path) = path {
                    self.new_task(path)
                } else {
                    iced::Command::none()
                }
            }
            MyMessage::Event(event) => {
                match event {
                    iced::Event::Window(w) => match w {
                        iced::window::Event::FileDropped(path) => {
                            if self.is_working() {
                                log::info!(target: LOG_TARGET, "File dropped on application window, but already working, so file will be ignored. File path: {:?}", path);
                                return iced::Command::none();
                            }
                            log::info!(target: LOG_TARGET, "File dropped on application window: {:?}", path);
                            return self.new_task(path);
                        }
                        _ => {}
                    },
                    _ => {}
                }
                iced::Command::none()
            }
            MyMessage::CancelMessageSentToWorker => {
                log::info!(target: LOG_TARGET, "Cancel command sent to worker");
                if self.idle {
                    self.clear_all();
                }
                iced::Command::none()
            }
            MyMessage::GifFramesLoaded(result) => {
                // NOTE: Ideally, the GIF processing job could be cancelled
                // if not ready when clearing, to avoid populating `error_messsage` upon error
                // or `frames` when the job has been cancelled or cleared.
                // For now we take care of "Ok", but not "Err".
                log::debug!(target: LOG_TARGET, "Animated GIF 'frames loaded' message recevied.");
                match result {
                    Err(e) => {
                        log::warn!(target: LOG_TARGET, "Error preparing GIF frames: {:?}", e);
                        self.error_message = Some(e.to_string());
                    }
                    Ok(frames) => {
                        if self.image_data.is_some() {
                            self.frames = Some(frames);
                        } else {
                            log::warn!(target: LOG_TARGET, "Received GIF frames but image data is no longer there, so assuming the job has been cleared and ignoring the result.");
                        }
                    }
                }
                iced::Command::none()
            }
            MyMessage::WorkerEvent(event) => match event {
                worker::Event::CommandRefused(refused_command) => {
                    log::error!(target: LOG_TARGET, "Command was refused by worker: {:?}", refused_command);
                    panic!();
                }
                worker::Event::TaskRefused(refused_task_settings) => {
                    log::error!(target: LOG_TARGET, "New task was refused by worker: {:?}", refused_task_settings);
                    panic!();
                }
                worker::Event::Ready(tx) => {
                    log::info!(target: LOG_TARGET, "Worker is ready (received 'command sender' channel)");
                    self.tx = Some(tx);
                    iced::Command::none()
                }
                worker::Event::Message(message) => match message {
                    ffmpeg_gif_maker::Message::Done => {
                        // IMPORTANT: Rely on this message instead of 'success' or 'error' to mark the job as completed.
                        log::info!(target: LOG_TARGET, "'Done' message received.");
                        self.idle = true;
                        let Some(image_data) = self.image_data.as_ref() else {
                            // NOTE: This edge case could be eliminated by introducing
                            // a cancellation channel in the `gif::Frames::from_bytes`
                            // method.
                            log::warn!(target: LOG_TARGET, "There was no image data, so not requesting GIF preview.");
                            return iced::Command::none();
                        };
                        let data = image_data.clone();
                        log::debug!(target: LOG_TARGET, "Returning command that will initiate the GIF processing...");
                        return iced::Command::perform(
                            iced_gif::gif::Frames::from_bytes(data),
                            MyMessage::GifFramesLoaded,
                        );
                    }
                    ffmpeg_gif_maker::Message::Success(image_data) => {
                        log::debug!(target: LOG_TARGET, "Image data received from worker.");
                        self.image_data = Some(image_data);
                        iced::Command::none()
                    }
                    ffmpeg_gif_maker::Message::VideoDuration(duration) => {
                        log::debug!(target: LOG_TARGET, "Video duration received from worker: {:?}", duration);
                        self.video_duration = Some(duration);
                        iced::Command::none()
                    }
                    ffmpeg_gif_maker::Message::Progress(progress) => {
                        log::debug!(target: LOG_TARGET, "Progress received from worker: {:.2}", progress);
                        self.progress = Some(progress);
                        iced::Command::none()
                    }
                    ffmpeg_gif_maker::Message::Error(error) => {
                        log::warn!(target: LOG_TARGET, "Error received from worker: {:?}", error);
                        match error {
                            ffmpeg_gif_maker::Error::Cancelled => {
                                self.clear_all();
                            }
                            ffmpeg_gif_maker::Error::EmptyStdout => {
                                self.error_message = Some("Likely unsupported file format.".into());
                            }
                            e @ _ => {
                                self.error_message = Some(e.to_string());
                            }
                        }
                        iced::Command::none()
                    }
                },
            },
        }
    }
}

mod numeric_input {
    // [component example](https://github.com/iced-rs/iced/blob/master/examples/component/src/main.rs)

    pub trait Unsigned: ToString + std::str::FromStr {}
    impl Unsigned for u8 {}
    impl Unsigned for u16 {}
    impl Unsigned for u32 {}
    impl Unsigned for u64 {}
    impl Unsigned for u128 {}

    pub struct NumericInput<M, T>
    where
        T: Unsigned,
    {
        placeholder: Option<String>,
        value: Option<T>,
        on_change: Box<dyn Fn(Option<T>) -> M>,
        font: Option<iced::Font>,
        size: Option<iced::Pixels>,
        disabled: bool,
    }

    impl<M, T> NumericInput<M, T>
    where
        T: Unsigned,
    {
        pub fn new(value: Option<T>, on_change: impl Fn(Option<T>) -> M + 'static) -> Self {
            Self {
                placeholder: None,
                value,
                on_change: Box::new(on_change),
                font: None,
                size: None,
                disabled: false,
            }
        }

        pub fn placeholder(self, placeholder: impl Into<String>) -> Self {
            Self {
                placeholder: Some(placeholder.into()),
                ..self
            }
        }

        pub fn font(self, font: iced::Font) -> Self {
            Self {
                font: Some(font),
                ..self
            }
        }

        pub fn size(self, size: impl Into<iced::Pixels>) -> Self {
            Self {
                size: Some(size.into()),
                ..self
            }
        }

        pub fn disabled(self, disabled: bool) -> Self {
            Self { disabled, ..self }
        }
    }

    #[derive(Clone, Debug)]
    pub enum Event {
        InputChanged(String),
    }

    impl<M, T> iced::widget::Component<M, iced::Renderer> for NumericInput<M, T>
    where
        T: Unsigned,
    {
        type State = ();
        type Event = Event;

        fn update(&mut self, _state: &mut Self::State, event: Self::Event) -> Option<M> {
            match event {
                Event::InputChanged(s) => {
                    if s.is_empty() {
                        Some((self.on_change)(None))
                    } else {
                        s.parse().ok().map(Some).map(self.on_change.as_ref())
                    }
                }
            }
        }

        fn view(
            &self,
            _state: &Self::State,
        ) -> iced::advanced::graphics::core::Element<'_, Self::Event, iced::Renderer> {
            let input = iced::widget::text_input(
                self.placeholder.as_ref().unwrap_or(&String::from("")),
                self.value
                    .as_ref()
                    .map(T::to_string)
                    .as_deref()
                    .unwrap_or(""),
            );
            if !self.disabled {
                input.on_input(Event::InputChanged)
            } else {
                input
            }
            .width(iced::Length::Fixed(50.0))
            .padding([3.0, 4.0])
            .font(self.font.unwrap_or(Default::default()))
            .size(self.size.unwrap_or(16.into()))
            .into()
        }
    }

    impl<'a, M, T> std::convert::From<NumericInput<M, T>> for iced::Element<'a, M, iced::Renderer>
    where
        M: 'a,
        T: Unsigned + 'a,
    {
        fn from(value: NumericInput<M, T>) -> Self {
            iced::widget::component(value)
        }
    }
}
