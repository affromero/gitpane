use std::cmp::Ordering;

#[derive(Clone, Copy)]
pub(crate) enum Order {
    Alphabetical,
    ReverseAlphabetical,
    DirtyFirst,
}

pub(crate) struct Key {
    pub anchor_clean: bool,
    pub anchor: String,
    pub nested: bool,
    pub own: String,
}

pub(crate) fn compare(a: &Key, b: &Key, order: Order) -> Ordering {
    let anchor = match order {
        Order::ReverseAlphabetical => b.anchor.cmp(&a.anchor),
        _ => a.anchor.cmp(&b.anchor),
    };
    let own = match order {
        Order::ReverseAlphabetical => b.own.cmp(&a.own),
        _ => a.own.cmp(&b.own),
    };
    match order {
        Order::DirtyFirst => a.anchor_clean.cmp(&b.anchor_clean).then(anchor),
        _ => anchor,
    }
    .then(a.nested.cmp(&b.nested))
    .then(own)
}
