use super::Overlay;

#[test]
fn refresh_burst_reuses_the_pending_search_until_it_completes() {
    let mut inbox = Overlay {
        open: true,
        ..Overlay::default()
    };
    assert!(inbox.begin_list());
    assert!((0..8).all(|_| !inbox.begin_list()));
    assert!(!inbox.finish_list(false, Ok(Vec::new())));
    assert!(inbox.rows.as_ref().is_some_and(Result::is_ok));
    assert!(inbox.begin_list());
}

#[test]
fn a_tab_switch_only_requests_the_current_tab_while_the_inbox_is_open() {
    let mut inbox = Overlay {
        open: true,
        reviews: true,
        ..Overlay::default()
    };
    assert!(inbox.begin_list());
    assert!(!inbox.begin_list());
    assert!(inbox.finish_list(false, Err("search failed".to_owned())));
    assert!(inbox.rows.is_none());
    assert!(inbox.begin_list());
    inbox.close();
    assert!(!inbox.finish_list(true, Ok(Vec::new())));
    assert!(inbox.cached_reviews.is_some());
    assert!(inbox.rows.is_none());
}
