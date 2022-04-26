use r2d2_sqlite::rusqlite::Connection;
use std::collections::HashSet;
use std::sync::{Arc, RwLock};

#[derive(Hash, Eq, PartialEq)]
struct SearchItem {
    name: String,
    calories: i64,
}

#[derive(Clone)]
pub struct Searcher(Arc<SearcherInner>);

struct SearcherInner {
    matcher: Arc<fuzzy_matcher::skim::SkimMatcherV2>,
    items: Arc<RwLock<HashSet<SearchItem>>>,
}

impl Searcher {
    pub fn new(c: &Connection) -> Self {}
}
