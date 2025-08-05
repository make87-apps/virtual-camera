use make87::encodings::{Encoder, ProtobufEncoder};
use make87::interfaces::zenoh::ZenohInterface;
use make87_messages::core::Header;
use make87_messages::google::protobuf::Timestamp;
use make87_messages::text::PlainText;
use std::error::Error;
use std::time::Duration;
use tokio::time::interval;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    env_logger::init();

    let message_encoder = ProtobufEncoder::<PlainText>::new();
    let zenoh_interface = ZenohInterface::from_default_env("zenoh")?;
    let session = zenoh_interface.get_session().await?;

    let publisher = zenoh_interface
        .get_publisher(&session, "outgoing_message")
        .await?;

    let mut ticker = interval(Duration::from_millis(1000));

    loop {
        ticker.tick().await;
        let message = PlainText {
            header: Some(Header {
                timestamp: Timestamp::get_current_time().into(),
                reference_id: 0,
                entity_path: "/".to_string(),
            }),
            body: "Hello, World! 🦀".to_string(),
        };
        let message_encoded = message_encoder.encode(&message)?;
        publisher.put(&message_encoded).await?;

        log::info!("Published: {:?}", message);
    }
}
