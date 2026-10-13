use std::sync::Arc;

use cosmic::iced::Subscription;
use ferese_ipc::activities::Snapshot;

pub(crate) fn subscription() -> Subscription<Arc<Snapshot>> {
    Subscription::run(stream)
}

fn stream() -> impl cosmic::iced::futures::Stream<Item = Arc<Snapshot>> {
    crate::ipc_subscription::stream("activity-get", "activity-watch", |snapshot: &Snapshot| {
        snapshot.revision
    })
}
