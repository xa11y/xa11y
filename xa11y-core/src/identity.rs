//! Platform identity of an element, for equality.
//!
//! An [`Element`](crate::Element) is the reference to a node: its actions
//! reach that node or fail with [`Error::ElementStale`](crate::Error), and
//! never re-match by name. [`NodeIdentity`] answers the other question a
//! holder of two elements has: are they the same node?
//!
//! An identity has two parts:
//!
//! - a **process key**, which changes when the owning process is replaced
//!   (pid plus process creation time on Windows, the pid on macOS, the D-Bus
//!   unique bus name on Linux), and
//! - a **native key**, which names the node inside that process (UIA
//!   `RuntimeId`, the retained `AXUIElementRef` compared with `CFEqual`, the
//!   AT-SPI object path).
//!
//! Identity is only meaningful within one session: it is not serializable
//! and is never promised across a process restart. Platforms can reuse a
//! native key after its node is destroyed (UIA runtime ids, AppKit object
//! tokens, Qt object paths), so two identities taken at different times can
//! compare equal for different nodes; the liveness check before every action
//! is what catches that case.

use std::any::Any;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A native platform object that can be compared for identity without being
/// representable as a string: on macOS, a retained `AXUIElementRef` compared
/// with `CFEqual` and hashed with `CFHash`.
///
/// Implemented by backends; consumers only see [`NodeIdentity`].
pub trait NativeObject: Send + Sync + fmt::Debug {
    /// A hash consistent with [`same_object`](Self::same_object).
    fn object_hash(&self) -> u64;
    /// Whether `other` is the same platform object.
    fn same_object(&self, other: &dyn NativeObject) -> bool;
    /// For downcasting in [`same_object`](Self::same_object).
    fn as_any(&self) -> &dyn Any;
}

#[derive(Debug, Clone)]
enum NativeKey {
    Key(String),
    Object(Arc<dyn NativeObject>),
}

/// The platform identity of one node within one session. Equal identities
/// name the same node. See the [module docs](self).
///
/// Built by [`Provider::identity`](crate::Provider::identity); compare with
/// `==` or [`Element::same_as`](crate::Element::same_as).
#[derive(Debug, Clone)]
pub struct NodeIdentity {
    backend: &'static str,
    process: String,
    native: NativeKey,
}

impl NodeIdentity {
    /// An identity whose native key is a string (a UIA runtime id, an AT-SPI
    /// object path). `backend` keeps identities from different sources (UIA
    /// and IA2) from ever comparing equal.
    pub fn from_key(
        backend: &'static str,
        process: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            process: process.into(),
            native: NativeKey::Key(key.into()),
        }
    }

    /// An identity whose native key is a platform object compared by the
    /// platform's own equality.
    pub fn from_object(
        backend: &'static str,
        process: impl Into<String>,
        object: Arc<dyn NativeObject>,
    ) -> Self {
        Self {
            backend,
            process: process.into(),
            native: NativeKey::Object(object),
        }
    }

    /// A hash consistent with `==`, for binding `__hash__` implementations.
    pub fn hash_u64(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }
}

impl PartialEq for NodeIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.backend == other.backend
            && self.process == other.process
            && match (&self.native, &other.native) {
                (NativeKey::Key(a), NativeKey::Key(b)) => a == b,
                (NativeKey::Object(a), NativeKey::Object(b)) => a.same_object(b.as_ref()),
                _ => false,
            }
    }
}

impl Eq for NodeIdentity {}

impl Hash for NodeIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.backend.hash(state);
        self.process.hash(state);
        match &self.native {
            NativeKey::Key(k) => {
                0u8.hash(state);
                k.hash(state);
            }
            NativeKey::Object(o) => {
                1u8.hash(state);
                o.object_hash().hash(state);
            }
        }
    }
}

impl fmt::Display for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.native {
            NativeKey::Key(k) => write!(f, "{}:{}:{}", self.backend, self.process, k),
            NativeKey::Object(o) => write!(
                f,
                "{}:{}:#{:016x}",
                self.backend,
                self.process,
                o.object_hash()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[derive(Debug)]
    struct Token(u32);
    impl NativeObject for Token {
        fn object_hash(&self) -> u64 {
            u64::from(self.0 % 2) // deliberately colliding
        }
        fn same_object(&self, other: &dyn NativeObject) -> bool {
            other
                .as_any()
                .downcast_ref::<Token>()
                .is_some_and(|o| o.0 == self.0)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn keys_compare_by_backend_process_and_key() {
        let a = NodeIdentity::from_key("uia", "1:100", "42.1");
        assert_eq!(a, NodeIdentity::from_key("uia", "1:100", "42.1"));
        assert_ne!(a, NodeIdentity::from_key("ia2", "1:100", "42.1"));
        assert_ne!(a, NodeIdentity::from_key("uia", "1:101", "42.1"));
        assert_ne!(a, NodeIdentity::from_key("uia", "1:100", "42.2"));
    }

    #[test]
    fn objects_use_platform_equality_even_when_hashes_collide() {
        let a = NodeIdentity::from_object("ax", "7", Arc::new(Token(1)));
        let b = NodeIdentity::from_object("ax", "7", Arc::new(Token(1)));
        let c = NodeIdentity::from_object("ax", "7", Arc::new(Token(3)));
        assert_eq!(a, b);
        assert_eq!(a.hash_u64(), b.hash_u64());
        assert_eq!(
            a.hash_u64(),
            c.hash_u64(),
            "the test tokens collide on purpose"
        );
        assert_ne!(a, c);
        let set: HashSet<_> = [a, b, c].into_iter().collect();
        assert_eq!(set.len(), 2);
    }
}
