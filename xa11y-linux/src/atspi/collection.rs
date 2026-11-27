//! Read ahead for exhaustive AT-SPI walks. Collection supplies addresses only.
//! The walk still starts at its actual root and visits real GetChildren edges,
//! including children in other processes, in the provider's document order.

use super::{AccessibleRef, LinuxProvider, QueryWalk};
use futures_util::{stream, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use xa11y_core::{Deadline, Error, Result};
use zbus::zvariant::OwnedObjectPath;

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const MAX_HINTS: usize = 512;
const IN_FLIGHT_NODES: usize = 32;

#[derive(Default)]
pub(super) struct QueryPrefetch {
    nodes: HashMap<AccessibleRef, PrefetchedNode>,
}

struct PrefetchedNode {
    role: Option<u32>,
    name: Option<String>,
    children: Option<Vec<AccessibleRef>>,
}

impl QueryPrefetch {
    pub(super) fn role(&self, provider: &LinuxProvider, node: &AccessibleRef) -> Option<u32> {
        self.nodes
            .get(node)
            .and_then(|r| r.role)
            .or_else(|| provider.get_role_number(node).ok())
    }

    pub(super) fn name(&self, node: &AccessibleRef) -> Option<Option<String>> {
        self.nodes
            .get(node)?
            .name
            .as_ref()
            .map(|name| (!name.is_empty()).then(|| name.clone()))
    }

    pub(super) fn children(
        &self,
        provider: &LinuxProvider,
        node: &AccessibleRef,
    ) -> Result<Option<Vec<AccessibleRef>>> {
        match self.nodes.get(node).and_then(|r| r.children.as_ref()) {
            Some(children) => Ok(Some(children.clone())),
            None => provider.live_atspi_children(node),
        }
    }
}

impl LinuxProvider {
    pub(super) fn query_prefetch(
        &self,
        root: &AccessibleRef,
        walk: &QueryWalk<'_>,
    ) -> QueryPrefetch {
        let mut result = QueryPrefetch::default();
        if walk.limit.is_some()
            || walk.max_depth != xa11y_core::MAX_TREE_DEPTH
            || xa11y_core::Deadline::current().is_some()
            || root.bus_name == "org.a11y.atspi.Registry"
        {
            return result;
        }
        // A short independent budget bounds speculative work even if an
        // unreachable object is slow. Only completed observations are used.
        let deadline = Deadline::after(Duration::from_millis(60));
        if let Err(e) = deadline
            .clone()
            .scope(|| self.prefetch_nodes(root, walk, &deadline, &mut result))
        {
            // Discovery is optional. The authoritative reads on reachable nodes
            // below still diagnose an inaccessible, stale or unresponsive root.
            log::debug!(
                "Collection discovery unavailable on {}{}: {e}; walking child links",
                root.bus_name,
                root.path
            );
        }
        result
    }

    fn prefetch_nodes(
        &self,
        root: &AccessibleRef,
        walk: &QueryWalk<'_>,
        deadline: &Deadline,
        result: &mut QueryPrefetch,
    ) -> Result<()> {
        let hints = self.collection_hints(root)?;
        if hints.is_empty() {
            return Ok(());
        }
        let conn = self.connection()?;
        let need_name = walk
            .clauses
            .iter()
            .any(|clause| clause.filters.iter().any(|f| f.attr == "name"));
        async_io::block_on(futures_lite::future::or(
            async {
                let mut reads = stream::iter(hints)
                    .map(|node| read_node(conn.inner(), node, need_name))
                    .buffer_unordered(IN_FLIGHT_NODES);
                while let Some((node, record)) = reads.next().await {
                    result.nodes.insert(node, record);
                }
            },
            async {
                async_io::Timer::after(deadline.remaining()).await;
            },
        ));
        Ok(())
    }

    fn collection_hints(&self, root: &AccessibleRef) -> Result<Vec<AccessibleRef>> {
        if !self.get_interfaces(root)?.collection {
            return Ok(Vec::new());
        }
        let proxy = self.make_proxy(&root.bus_name, &root.path, "org.a11y.atspi.Collection")?;
        // Empty sets with MATCH_ALL admit every role, including unknown and
        // toolkit-refined roles. A truncated packet simply gives fewer hints.
        let all = (
            vec![0i32; 2],
            1i32,
            HashMap::<String, String>::new(),
            1i32,
            vec![0i32; 4],
            1i32,
            Vec::<String>::new(),
            1i32,
            false,
        );
        let reply = proxy
            .call_method("GetMatches", &(all, 1u32, MAX_HINTS as i32, true))
            .map_err(|e| Error::platform(-1, format!("Collection.GetMatches failed: {e}")))?;
        let refs: Vec<(String, OwnedObjectPath)> = reply.body().deserialize().map_err(|e| {
            Error::platform(-1, format!("Collection.GetMatches decode failed: {e}"))
        })?;
        Ok(refs
            .into_iter()
            .take(MAX_HINTS)
            .filter(|(bus, _)| bus == &root.bus_name)
            .map(AccessibleRef::from)
            .filter(|r| !r.is_null())
            .collect())
    }
}

async fn read_node(
    conn: &zbus::Connection,
    node: AccessibleRef,
    need_name: bool,
) -> (AccessibleRef, PrefetchedNode) {
    let call = |method| {
        conn.call_method(
            Some(node.bus_name.as_str()),
            node.path.as_str(),
            Some(ACCESSIBLE),
            method,
            &(),
        )
    };
    // Unread fields stay absent in this speculative record. A reachable
    // node then takes the normal read and error path for that field.
    let name = async {
        if !need_name {
            return None;
        }
        conn.call_method(
            Some(node.bus_name.as_str()),
            node.path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(ACCESSIBLE, "Name"),
        )
        .await
        .and_then(|m| m.body().deserialize::<zbus::zvariant::OwnedValue>())
        .ok()
        .and_then(|v| String::try_from(v).ok())
    };
    let ((role, children), name) = futures_lite::future::zip(
        futures_lite::future::zip(call("GetRole"), call("GetChildren")),
        name,
    )
    .await;
    let role = role.and_then(|m| m.body().deserialize()).ok();
    let children = children
        .and_then(|m| m.body().deserialize::<Vec<(String, OwnedObjectPath)>>())
        .ok()
        .map(|refs| refs.into_iter().map(AccessibleRef::from).collect());
    (
        node,
        PrefetchedNode {
            role,
            name,
            children,
        },
    )
}
