use fuzzy_matcher::skim::SkimMatcherV2;
use r2d2_sqlite::rusqlite::Connection;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

pub struct SearchItem {
    pub name: String,
    pub calories: f64,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub name: String,
    pub calories: f64,
    pub positions: Vec<u32>,
}

#[derive(Clone)]
pub struct Searcher(Arc<SearcherInner>);

struct SearcherInner {
    matcher: SkimMatcherV2,
    items: RwLock<BTreeMap<u64, SearchItem>>,
}

impl Searcher {
    pub fn new(c: &Connection) -> Self {
        let mut qry = c
            .prepare("SELECT id, name, calories FROM items")
            .expect("could not prepare qry");
        let mut rows = qry.query([]).expect("could not get rows");

        let mut items = BTreeMap::new();
        while let Ok(Some(row)) = rows.next() {
            items.insert(
                row.get_unwrap("id"),
                SearchItem {
                    name: row.get_unwrap("name"),
                    calories: row.get_unwrap("calories"),
                },
            );
        }

        Self(Arc::new(SearcherInner {
            matcher: SkimMatcherV2::default().ignore_case(),
            items: RwLock::new(items),
        }))
    }

    pub fn update(&self, id: u64, name: Option<String>, calories: Option<f64>) {
        self.0
            .items
            .write()
            .expect("could not lock write")
            .get_mut(&id)
            .map(move |x| {
                if let Some(name) = name {
                    x.name = name;
                }
                if let Some(calories) = calories {
                    x.calories = calories;
                }
            });
    }

    pub fn insert(&self, id: u64, item: SearchItem) {
        self.0
            .items
            .write()
            .expect("could not lock write")
            .insert(id, item);
    }

    pub fn remove(&self, id: u64) {
        self.0
            .items
            .write()
            .expect("could not lock write")
            .remove(&id);
    }

    pub fn search(&self, qry: &str) -> Vec<SearchResult> {
        let items = self.0.items.read().expect("could not lock read");
        let mut results = vec![];
        let mut seen = HashSet::new();
        for (&id, item) in items.iter().rev() {
            if item.name.len() == 0 || !seen.insert(&*item.name) {
                continue;
            }
            let res = self.0.matcher.fuzzy(&*item.name, qry, true);
            if res.is_none() {
                continue;
            }
            let (score, pos) = res.unwrap();
            results.push((score, id, pos));
        }
        results.sort_unstable_by_key(|(score, id, _)| (-*score, !*id));
        results
            .into_iter()
            .take(5)
            .map(|(_, id, pos)| {
                let item = items.get(&id).unwrap();
                SearchResult {
                    name: item.name.clone(),
                    calories: item.calories,
                    positions: pos,
                }
            })
            .collect()
    }
}
