//! Ordered, unique-key VNI maps with support for arbitrary serialized keys.
//!
//! Serde's JSON object representation cannot carry fixed arrays or structs as
//! keys. VNX CODE_MAP can, so this wrapper serializes as a sorted sequence of
//! `[key, value]` pairs. It also accepts ordinary JSON objects when the key's
//! deserializer supports a string object key (strings, integers and enums).
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{btree_map, BTreeMap};
use std::fmt;
use std::iter::FromIterator;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

/// A VNI `map<K,V>` with C++ `std::map` ordering and unique-key semantics.
///
/// Deserialization retains the last value of each duplicate key, matching the
/// native VNX CODE_MAP reader. Construction through `insert`, `extend` and
/// `collect` has the same behavior. (The C++ CODE_LIST conversion is a distinct
/// compatibility path that retains the first duplicate instead.)
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Map<K: Ord, V>(BTreeMap<K, V>);

impl<K: Ord, V> Map<K, V> {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }
    pub fn into_inner(self) -> BTreeMap<K, V> {
        self.0
    }
}
// Deriving Default would unnecessarily require defaults for keys and values.
impl<K: Ord, V> Default for Map<K, V> {
    fn default() -> Self {
        Self::new()
    }
}
impl<K: Ord, V> Deref for Map<K, V> {
    type Target = BTreeMap<K, V>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<K: Ord, V> DerefMut for Map<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl<K: Ord, V> AsRef<BTreeMap<K, V>> for Map<K, V> {
    fn as_ref(&self) -> &BTreeMap<K, V> {
        &self.0
    }
}
impl<K: Ord, V> AsMut<BTreeMap<K, V>> for Map<K, V> {
    fn as_mut(&mut self) -> &mut BTreeMap<K, V> {
        &mut self.0
    }
}
impl<K: Ord, V> From<BTreeMap<K, V>> for Map<K, V> {
    fn from(map: BTreeMap<K, V>) -> Self {
        Self(map)
    }
}
impl<K: Ord, V> From<Map<K, V>> for BTreeMap<K, V> {
    fn from(map: Map<K, V>) -> Self {
        map.0
    }
}
impl<K: Ord, V, const N: usize> From<[(K, V); N]> for Map<K, V> {
    fn from(pairs: [(K, V); N]) -> Self {
        pairs.into_iter().collect()
    }
}
impl<K: Ord, V> FromIterator<(K, V)> for Map<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(pairs: T) -> Self {
        Self(pairs.into_iter().collect())
    }
}
impl<K: Ord, V> Extend<(K, V)> for Map<K, V> {
    fn extend<T: IntoIterator<Item = (K, V)>>(&mut self, pairs: T) {
        self.0.extend(pairs);
    }
}
impl<'a, K: Ord + Copy + 'a, V: Copy + 'a> Extend<(&'a K, &'a V)> for Map<K, V> {
    fn extend<T: IntoIterator<Item = (&'a K, &'a V)>>(&mut self, pairs: T) {
        self.0.extend(pairs);
    }
}
impl<K: Ord, V> IntoIterator for Map<K, V> {
    type Item = (K, V);
    type IntoIter = btree_map::IntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
impl<'a, K: Ord, V> IntoIterator for &'a Map<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = btree_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
impl<'a, K: Ord, V> IntoIterator for &'a mut Map<K, V> {
    type Item = (&'a K, &'a mut V);
    type IntoIter = btree_map::IterMut<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}
impl<K: Ord + Serialize, V: Serialize> Serialize for Map<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter())
    }
}
impl<'de, K: Ord + Deserialize<'de>, V: Deserialize<'de>> Deserialize<'de> for Map<K, V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<K: Ord, V>(PhantomData<(K, V)>);
        impl<'de, K: Ord + Deserialize<'de>, V: Deserialize<'de>> Visitor<'de> for MapVisitor<K, V> {
            type Value = Map<K, V>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence of [key, value] pairs or a JSON object")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut output = Map::new();
                while let Some((key, value)) = input.next_element::<(K, V)>()? {
                    output.insert(key, value);
                }
                Ok(output)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut output = Map::new();
                while let Some((key, value)) = input.next_entry::<K, V>()? {
                    output.insert(key, value);
                }
                Ok(output)
            }
        }
        deserializer.deserialize_any(MapVisitor(PhantomData))
    }
}
