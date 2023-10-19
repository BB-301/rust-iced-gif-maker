use ffmpeg_gif_maker::{Command, CommandSender, Converter, Message, MessageReceiver, Settings};
use iced::futures::channel::mpsc;
use iced::futures::sink::SinkExt;

const LOG_TARGET: &'static str = "iced_gif_maker::worker";

#[derive(Clone, Debug)]
pub enum Event {
    Ready(mpsc::Sender<Input>),
    Message(Message),
    CommandRefused(Command),
    TaskRefused(Settings),
}

#[derive(Debug)]
pub enum Input {
    Command(Command),
    NewTask(Settings),
}

pub enum State {
    Starting,
    Ready(mpsc::Receiver<Input>),
}

const CHANNEL_SIZE: usize = 100;

pub fn worker() -> iced::Subscription<Event> {
    struct Worker;

    iced::subscription::channel(
        std::any::TypeId::of::<Worker>(),
        CHANNEL_SIZE,
        |mut my_output| async move {
            let mut state = State::Starting;
            let mut rx: Option<MessageReceiver> = None;
            let mut tx: Option<CommandSender> = None;

            log::debug!(target: LOG_TARGET, "Entering main loop...");

            loop {
                log::debug!(target: LOG_TARGET, "New main loop iteration...");
                match &mut state {
                    State::Starting => {
                        log::debug!(target: LOG_TARGET, "Entered state's STARTING branch. Creating channel...");

                        let (sender, receiver) = mpsc::channel(CHANNEL_SIZE);
                        if let Err(e) = my_output.send(Event::Ready(sender)).await {
                            log::error!(target: LOG_TARGET, "Failed to send message to app {:?}", e);
                            panic!();
                        }
                        log::debug!(target: LOG_TARGET, "Channel created and sender part sent to app.");
                        state = State::Ready(receiver);
                    }
                    State::Ready(receiver) => {
                        log::debug!(target: LOG_TARGET, "Entered state's READY branch.");

                        use iced::futures::StreamExt;

                        if let (Some(message_rx), Some(command_tx)) = (rx.as_mut(), tx.as_ref()) {
                            log::debug!(target: LOG_TARGET, "Converter channels present, so entering job loop...");

                            loop {
                                tokio::select! {
                                    input = receiver.select_next_some() => {
                                        match input {
                                            Input::Command(command) => {
                                                log::debug!(target: LOG_TARGET, "Received command from application. Transfering it to FFmpeg converter...");
                                                if let Err(e) = command_tx.send(command) {
                                                    log::warn!(target: LOG_TARGET, "Failed to send command to converter: {:?}", e);
                                                }
                                            }
                                            Input::NewTask(settings) => {
                                                if let Err(e) = my_output.send(Event::TaskRefused(settings)).await {
                                                    log::error!(target: LOG_TARGET, "Failed to send event: {:?}", e);
                                                    panic!();
                                                }
                                            }
                                        }
                                    },
                                    message = message_rx.recv() => match message {
                                        Some(message) => {
                                            log::debug!(target: LOG_TARGET, "Received command message from converter (see 'trace' for details)");
                                            log::trace!(target: LOG_TARGET, "Mesage\n{:?}", message);
                                            let should_break = if let Message::Done = &message { true } else { false };
                                            if let Err(e) = my_output.send(Event::Message(message)).await {
                                                log::error!(target: LOG_TARGET, "Failed to send event message: {:?}", e);
                                                panic!();
                                            }
                                            if should_break {
                                                log::debug!(target: LOG_TARGET, "Converter sent DONE message, so breaking out of loop...");
                                                break;
                                            }
                                        }
                                        None => {
                                            log::debug!(target: LOG_TARGET, "rx_message has closed");
                                            break;
                                        }
                                    }
                                };
                            }
                            log::debug!(target: LOG_TARGET, "Releasing the converter channels...");
                            rx = None;
                            tx = None;
                        } else {
                            log::debug!(target: LOG_TARGET, "Converter channels not present, so waiting for input from application...");

                            let input = receiver.select_next_some().await;

                            log::debug!(target: LOG_TARGET, "Input received from application: {:?}", input);

                            let settings = match input {
                                Input::NewTask(settings) => settings,
                                Input::Command(command) => {
                                    log::warn!(target: LOG_TARGET, "Command refused because no conversion job exists: {:?}", command);
                                    if let Err(e) =
                                        my_output.send(Event::CommandRefused(command)).await
                                    {
                                        log::error!(target: LOG_TARGET, "Failed to send event to application: {:?}", e);
                                        panic!();
                                    }
                                    continue;
                                }
                            };

                            log::debug!(target: LOG_TARGET, "Instantiating converter and associated channels...");
                            let (converter, sender, receiver) = Converter::new_with_channels();

                            log::debug!(target: LOG_TARGET, "Spawning thread for conversion job...");
                            std::thread::spawn(move || {
                                log::debug!(target: LOG_TARGET, "Running conversion job...");
                                converter.convert(settings);
                            });

                            log::debug!(target: LOG_TARGET, "Storing converter channels...");
                            rx = Some(receiver);
                            tx = Some(sender);
                        }
                    }
                }
            }
        },
    )
}
