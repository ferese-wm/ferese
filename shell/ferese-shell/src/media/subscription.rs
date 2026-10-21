use std::sync::Arc;

use cosmic::iced::Subscription;
use ferese_ipc::media::Snapshot;

pub(super) fn subscription() -> Subscription<Arc<Snapshot>> {
    Subscription::run(stream)
}

fn stream() -> impl cosmic::iced::futures::Stream<Item = Arc<Snapshot>> {
    crate::ipc_subscription::stream("media-get", "media-watch", |snapshot: &Snapshot| {
        snapshot.revision
    })
}
