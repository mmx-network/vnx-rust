use crate::{binary, error::invalid, Registry, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::ops::{Deref, DerefMut};

/// Implemented by generated VNI types. Rust serde is used for the object view;
/// the registry supplies VNX's independently versioned binary field layout.
pub trait VnxType: Serialize + DeserializeOwned + Default {
    const TYPE_NAME: &'static str;
    const TYPE_HASH: u64;
    const CODE_HASH: u64;
    fn type_name() -> &'static str {
        Self::TYPE_NAME
    }
    fn to_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        if let Some(object) = value.as_object_mut() {
            object.insert("__type".into(), Self::TYPE_NAME.into());
        } else {
            value = serde_json::json!({"__type":Self::TYPE_NAME,"value":value});
        }
        Ok(value)
    }
    fn from_value(mut value: Value) -> Result<Self> {
        if let Some(object) = value.as_object_mut() {
            if let Some(name) = object.remove("__type") {
                if name.as_str() != Some(Self::TYPE_NAME) {
                    return Err(invalid(format!("expected {}, got {name}", Self::TYPE_NAME)));
                }
            }
        }
        Ok(serde_json::from_value(value)?)
    }
    fn encode(&self, registry: &Registry) -> Result<Vec<u8>> {
        let mut writer = binary::Writer::new(Vec::new(), registry.clone());
        writer.write_typed(&self.to_value()?, Self::CODE_HASH)?;
        Ok(writer.into_inner())
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        let (v, _) = binary::decode(bytes)?;
        Self::from_value(v)
    }
}

/// Fixed VNI arrays, including byte arrays longer than serde's built-in support.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fixed<T, const N: usize>(pub [T; N]);
impl<T: Default, const N: usize> Default for Fixed<T, N> {
    fn default() -> Self {
        Self(std::array::from_fn(|_| T::default()))
    }
}
impl<T, const N: usize> Deref for Fixed<T, N> {
    type Target = [T; N];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<T, const N: usize> DerefMut for Fixed<T, N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl<T, const N: usize> From<[T; N]> for Fixed<T, N> {
    fn from(v: [T; N]) -> Self {
        Self(v)
    }
}
impl<T: Serialize, const N: usize> Serialize for Fixed<T, N> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.as_slice().serialize(s)
    }
}
impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for Fixed<T, N> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let values = Vec::<T>::deserialize(d)?;
        let array = values.try_into().map_err(|v: Vec<T>| {
            serde::de::Error::custom(format!("expected {N} elements, got {}", v.len()))
        })?;
        Ok(Self(array))
    }
}
pub fn object_type(value: &Value) -> Result<&str> {
    value
        .get("__type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("object has no __type"))
}
