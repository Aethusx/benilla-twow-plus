//! The client databases ClassicAPI reads, raw. The DLL reads the engine's in-memory records at
//! byte offsets (`Offsets.h`); a record there is the `WDBC` row with string offsets turned into
//! pointers, so byte offset / 4 is the column here. Each table is opened from the player's own
//! patch chain the first time something asks for it.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use benilla_formats::Chain;

/// The eight locale slots of a localized string column, then its flags dword.
pub const LOCALES: usize = 8;

/// One `WDBC` file: its records by id (column 0) and its string block.
pub struct Dbc {
    fields: usize,
    cells: Vec<u32>,
    strings: Vec<u8>,
    by_id: HashMap<u32, usize>,
}

impl Dbc {
    /// Parse `bytes`; `None` for a file that is not `WDBC` or is cut short.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let word = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        if bytes.get(0..4)? != b"WDBC" {
            return None;
        }
        let records = word(4)? as usize;
        let fields = word(8)? as usize;
        let record_size = word(12)? as usize;
        let string_size = word(16)? as usize;
        if fields == 0 || record_size != fields * 4 {
            return None;
        }
        let body = 20;
        let strings_at = body + records * record_size;
        let cells: Vec<u32> = bytes
            .get(body..strings_at)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        let strings = bytes.get(strings_at..strings_at + string_size)?.to_vec();
        let by_id = (0..records).map(|r| (cells[r * fields], r)).collect();
        Some(Self {
            fields,
            cells,
            strings,
            by_id,
        })
    }

    /// A table built from rows of cells, for the tests.
    #[cfg(test)]
    pub fn from_rows(rows: &[Vec<u32>], strings: &[u8]) -> Self {
        let fields = rows.first().map_or(1, Vec::len);
        let cells: Vec<u32> = rows.iter().flat_map(|r| r.iter().copied()).collect();
        let by_id = (0..rows.len()).map(|r| (cells[r * fields], r)).collect();
        Self {
            fields,
            cells,
            strings: strings.to_vec(),
            by_id,
        }
    }

    pub fn field_count(&self) -> usize {
        self.fields
    }

    pub fn len(&self) -> usize {
        self.cells.len() / self.fields
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The highest id, the engine's record count (`VAR_*_COUNT` is the max id).
    pub fn max_id(&self) -> u32 {
        self.by_id.keys().copied().max().unwrap_or(0)
    }

    /// The record with id `id`; id 0 never resolves, as the engine's lookups refuse it.
    pub fn row(&self, id: u32) -> Option<Row<'_>> {
        if id == 0 {
            return None;
        }
        let r = *self.by_id.get(&id)?;
        Some(self.nth(r))
    }

    /// The `r`-th record in file order.
    pub fn nth(&self, r: usize) -> Row<'_> {
        Row {
            dbc: self,
            cells: &self.cells[r * self.fields..(r + 1) * self.fields],
        }
    }

    /// Every record, in file order.
    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.cells
            .chunks_exact(self.fields)
            .map(|cells| Row { dbc: self, cells })
    }

    fn string(&self, offset: u32) -> Option<&str> {
        let tail = self.strings.get(offset as usize..)?;
        let end = tail.iter().position(|b| *b == 0)?;
        std::str::from_utf8(&tail[..end]).ok()
    }
}

/// One record.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    dbc: &'a Dbc,
    cells: &'a [u32],
}

impl<'a> Row<'a> {
    pub fn id(&self) -> u32 {
        self.u32(0)
    }

    pub fn u32(&self, col: usize) -> u32 {
        self.cells.get(col).copied().unwrap_or(0)
    }

    pub fn i32(&self, col: usize) -> i32 {
        self.u32(col) as i32
    }

    pub fn f32(&self, col: usize) -> f32 {
        f32::from_bits(self.u32(col))
    }

    /// Two cells as a little-endian `u64`, low dword first.
    pub fn u64(&self, col: usize) -> u64 {
        u64::from(self.u32(col)) | (u64::from(self.u32(col + 1)) << 32)
    }

    /// A string cell; empty for offset 0 or a bad offset.
    pub fn str(&self, col: usize) -> &'a str {
        self.dbc.string(self.u32(col)).unwrap_or("")
    }

    /// A localized string starting at `col`: the client's own locale slot, which on a
    /// single-locale install is the one slot that is filled.
    pub fn loc(&self, col: usize) -> &'a str {
        (0..LOCALES)
            .map(|i| self.str(col + i))
            .find(|s| !s.is_empty())
            .unwrap_or("")
    }
}

/// A parsed catalog, type-erased for the cache.
type AnyCatalog = Arc<dyn Any + Send + Sync>;

/// The tables and benilla-formats' parsed catalogs, each opened on first use; `None` when the
/// chain lacks it.
#[derive(Default)]
pub struct Databases {
    chain: OnceLock<Option<Mutex<Chain>>>,
    tables: Mutex<HashMap<&'static str, Option<Arc<Dbc>>>>,
    catalogs: Mutex<HashMap<TypeId, Option<AnyCatalog>>>,
    /// Tables a test seated, consulted before the chain.
    #[cfg(test)]
    seeded: Mutex<HashMap<&'static str, Arc<Dbc>>>,
}

impl Databases {
    fn chain(&self) -> Option<std::sync::MutexGuard<'_, Chain>> {
        self.chain
            .get_or_init(|| {
                let data = benilla_formats::wow_data()?;
                Chain::open(&data).ok().map(Mutex::new)
            })
            .as_ref()
            .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// A benilla-formats catalog, loaded by `load` the first time `T` is asked for.
    pub fn catalog<T, E>(&self, load: impl FnOnce(&mut Chain) -> Result<T, E>) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        let key = TypeId::of::<T>();
        if let Some(hit) = self
            .catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return hit.clone().and_then(|a| a.downcast::<T>().ok());
        }
        // Loaded with the cache unlocked: a loader never re-enters, but another table might.
        let loaded = self
            .chain()
            .and_then(|mut c| load(&mut c).ok())
            .map(Arc::new);
        self.catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, loaded.clone().map(|a| a as AnyCatalog));
        loaded
    }

    /// A value derived from the tables, computed by `build` the first time `T` is asked for; a
    /// `None` is not cached, so a build that ran before the data was there runs again.
    pub fn derived<T>(&self, build: impl FnOnce(&Self) -> Option<T>) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        let key = TypeId::of::<T>();
        if let Some(Some(hit)) = self
            .catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return hit.clone().downcast::<T>().ok();
        }
        let built = Arc::new(build(self)?);
        self.catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Some(built.clone() as AnyCatalog));
        Some(built)
    }

    /// Seat a catalog for a test.
    #[cfg(test)]
    pub fn seed_catalog<T: Send + Sync + 'static>(&self, value: T) {
        self.catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(TypeId::of::<T>(), Some(Arc::new(value) as AnyCatalog));
    }

    /// `DBFilesClient\<name>.dbc`, opened once.
    pub fn get(&self, name: &'static str) -> Option<Arc<Dbc>> {
        #[cfg(test)]
        if let Some(t) = self
            .seeded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
        {
            return Some(t.clone());
        }
        let mut tables = self.tables.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = tables.get(name) {
            return t.clone();
        }
        let table = self
            .chain()
            .and_then(|c| c.read(&format!("DBFilesClient\\{name}.dbc")).ok())
            .and_then(|b| Dbc::parse(&b))
            .map(Arc::new);
        tables.insert(name, table.clone());
        table
    }

    /// Seat a table for a test.
    #[cfg(test)]
    pub fn seed(&self, name: &'static str, table: Dbc) {
        self.seeded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, Arc::new(table));
    }

    /// A file off the chain, for the tables read whole (`ItemSubClass`'s companions, art).
    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.chain()?.read(path).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_localized_column_reads_the_filled_slot() {
        let strings = b"\0Feuerball\0";
        let mut row = vec![0u32; 12];
        row[0] = 133;
        row[1 + 3] = 1; // the deDE slot
        let t = Dbc::from_rows(&[row], strings);
        assert_eq!(t.row(133).unwrap().loc(1), "Feuerball");
        assert!(t.row(0).is_none());
        assert_eq!(t.max_id(), 133);
    }
}
