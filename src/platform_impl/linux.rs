use crate::{Route, RouteChange, Rule};
use std::io::{self, Error};

use async_stream::stream;
use futures::{channel::mpsc::UnboundedReceiver, stream::TryStreamExt};
use futures::{Stream, StreamExt};
use netlink_packet_core::{NetlinkMessage, NetlinkPayload};
use netlink_packet_route::rule::{RuleAttribute, RuleMessage};
use netlink_packet_route::{
    route::{RouteAddress, RouteAttribute, RouteMessage},
    AddressFamily, RouteNetlinkMessage,
};
use netlink_sys::{AsyncSocket, SocketAddr};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use tokio::{sync::broadcast, task::JoinHandle};

use rtnetlink::{
    constants::{RTMGRP_IPV4_ROUTE, RTMGRP_IPV6_ROUTE},
    new_connection,
};

pub struct Handle {
    handle: rtnetlink::Handle,
    join_handle: JoinHandle<()>,
    listen_handle: JoinHandle<()>,
    tx: broadcast::Sender<RouteChange>,
}

impl Handle {
    pub(crate) fn new() -> io::Result<Self> {
        let (mut connection, handle, messages) = new_connection()?;

        // These flags specify what kinds of broadcast messages we want to listen for.
        let mgroup_flags = RTMGRP_IPV4_ROUTE | RTMGRP_IPV6_ROUTE;

        // A netlink socket address is created with said flags.
        let addr = SocketAddr::new(0, mgroup_flags);
        // Said address is bound so new conenctions and thus new message broadcasts can be received.
        connection.socket_mut().socket_mut().bind(&addr)?;
        let (tx, _) = broadcast::channel::<RouteChange>(16);

        let join_handle = tokio::spawn(connection);
        let listen_handle = tokio::spawn(Self::listen(messages, tx.clone()));

        Ok(Self {
            handle,
            join_handle,
            listen_handle,
            tx,
        })
    }

    pub(crate) async fn default_route(&self) -> io::Result<Option<Route>> {
        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V4).execute();

        while let Some(route) = routes
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            if route.destination_prefix().is_none() {
                return Ok(Some(route.into()));
            }
        }

        let mut routes = self.handle.route().get(rtnetlink::IpVersion::V6).execute();

        while let Some(route) = routes
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            if route.destination_prefix().is_none() {
                return Ok(Some(route.into()));
            }
        }
        Ok(None)
    }

    pub(crate) async fn list_rules(&self) -> io::Result<Vec<RuleMessage>> {
        let mut rules = vec![];
        let mut rule_messages = self.handle.rule().get(rtnetlink::IpVersion::V4).execute();

        while let Some(rule) = rule_messages
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            rules.push(rule.into());
        }

        let mut rule_messages = self.handle.rule().get(rtnetlink::IpVersion::V6).execute();

        while let Some(rule) = rule_messages
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            rules.push(rule.into());
        }
        Ok(rules)
    }

    pub(crate) async fn add_rules(&self, rules: Vec<Rule>) -> io::Result<()> {
        for rule in rules {
            let mut req = self.handle.rule().add();
            // the default action is unspec, which doesn't work here
            req.message_mut().header.action = netlink_packet_route::rule::RuleAction::ToTable;
            // the src & dst is not supported in the common api, so we have to handle it manully
            if let Some(input_interface) = rule.input_interface {
                req = req.input_interface(input_interface);
            }
            if let Some(output_interface) = rule.output_interface {
                req = req.output_interface(output_interface);
            }
            if let Some(table_id) = rule.table_id {
                req = req.table_id(table_id);
            }
            if let Some(priority) = rule.priority {
                req = req.priority(priority);
            }
            if let Some((fw_mark, fw_mask)) = rule.fw_mark_mask {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::FwMark(fw_mark));
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::FwMask(fw_mask));
            }
            if let Some(suppress_prefixlength) = rule.suppress_prefixlength {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::SuppressPrefixLen(suppress_prefixlength));
            }
            if let Some(protocol) = rule.protocol {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::IpProtocol(protocol));
            }
            req = req.replace();
            if rule.v6 {
                let mut req = req.v6();
                if let Some((src, prefix)) = rule.src {
                    if let IpAddr::V6(src) = src {
                        req = req.source_prefix(src, prefix);
                    }
                }
                if let Some((dst, prefix)) = rule.dst {
                    if let IpAddr::V6(dst) = dst {
                        req = req.destination_prefix(dst, prefix);
                    }
                }
                req.execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?;
            } else {
                let mut req = req.v4();
                if let Some((src, prefix)) = rule.src {
                    if let IpAddr::V4(src) = src {
                        req = req.source_prefix(src, prefix);
                    }
                }
                if let Some((dst, prefix)) = rule.dst {
                    if let IpAddr::V4(dst) = dst {
                        req = req.destination_prefix(dst, prefix);
                    }
                }
                req.execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?;
            }
        }
        Ok(())
    }

    pub async fn delete_rules(&self, rules: Vec<Rule>) -> io::Result<()> {
        let mut failed = vec![];
        for rule in rules {
            let original_rule = rule.clone();
            let message = RuleMessage::default();
            let mut req = self.handle.rule().del(message);
            req.message_mut().header.action = netlink_packet_route::rule::RuleAction::ToTable;
            if let Some(src) = rule.src {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::Source(src.0));
                req.message_mut().header.src_len = src.1;
            }
            if let Some(dst) = rule.dst {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::Destination(dst.0));
                req.message_mut().header.dst_len = dst.1;
            }
            if let Some(ifname) = rule.input_interface {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::Iifname(ifname));
            }
            if let Some(ifname) = rule.output_interface {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::Oifname(ifname));
            }
            if let Some(table_id) = rule.table_id {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::Table(table_id));
            }
            if let Some(priority) = rule.priority {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::Priority(priority));
            }
            if let Some((fw_mark, fw_mask)) = rule.fw_mark_mask {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::FwMark(fw_mark));
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::FwMask(fw_mask));
            }
            if let Some(suppress_prefixlength) = rule.suppress_prefixlength {
                req.message_mut()
                    .attributes
                    .push(RuleAttribute::SuppressPrefixLen(suppress_prefixlength));
            }
            if rule.v6 {
                req.message_mut().header.family = AddressFamily::Inet6;
            } else {
                req.message_mut().header.family = AddressFamily::Inet;
            }
            match req.execute().await {
                Ok(_) => (),
                Err(e) => {
                    failed.push((original_rule, e));
                }
            }
        }
        if !failed.is_empty() {
            return Err(Error::new(
                io::ErrorKind::Other,
                format!("Failed to delete rules: {:?}", failed),
            ));
        }
        Ok(())
    }

    pub(crate) async fn list(&self) -> io::Result<Vec<Route>> {
        let mut routes = vec![];
        let mut route_messages = self.handle.route().get(rtnetlink::IpVersion::V4).execute();

        while let Some(route) = route_messages
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            routes.push(route.into());
        }

        let mut route_messages = self.handle.route().get(rtnetlink::IpVersion::V6).execute();

        while let Some(route) = route_messages
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            routes.push(route.into());
        }
        Ok(routes)
    }

    pub(crate) fn route_listen_stream(&self) -> impl Stream<Item = RouteChange> {
        let mut rx = self.tx.subscribe();
        stream! {
            loop {
                match rx.recv().await {
                    Ok(ev) => yield ev,
                    Err(e) => match e {
                        broadcast::error::RecvError::Closed => break,
                        broadcast::error::RecvError::Lagged(_) => continue,
                    }
                }
            }
        }
    }

    pub(crate) async fn delete(&self, route: &Route) -> io::Result<()> {
        let route_handle = self.handle.route();
        let mut routes = match route.destination {
            IpAddr::V4(_) => route_handle.get(rtnetlink::IpVersion::V4),
            IpAddr::V6(_) => route_handle.get(rtnetlink::IpVersion::V6),
        }
        .execute();

        while let Some(msg) = routes
            .try_next()
            .await
            .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?
        {
            let other_route: Route = msg.clone().into();
            if other_route.destination == route.destination
                && other_route.prefix == route.prefix
                && other_route.metric == route.metric
            {
                route_handle
                    .del(msg)
                    .execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))?;
                return Ok(());
            }
        }

        Err(Error::new(
            io::ErrorKind::NotFound,
            "No matching route found to delete",
        ))
    }

    pub(crate) async fn add(&self, route: &Route) -> io::Result<()> {
        let route_handle = self.handle.route();
        match route.destination {
            IpAddr::V4(addr) => {
                let mut msg = route_handle
                    .add()
                    .v4()
                    .table_id(route.table.into())
                    .destination_prefix(addr, route.prefix);

                if let Some(ifindex) = route.ifindex {
                    msg = msg.output_interface(ifindex);
                }

                if let Some(metric) = route.metric {
                    msg = msg.priority(metric);
                }

                if let Some(gateway) = route.gateway {
                    msg = match gateway {
                        IpAddr::V4(addr) => msg.gateway(addr),
                        IpAddr::V6(_) => {
                            return Err(Error::new(
                                io::ErrorKind::InvalidInput,
                                "gateway version must match destination",
                            ))
                        }
                    };
                }

                if let Some(src_hint) = route.source_hint {
                    msg = match src_hint {
                        IpAddr::V4(addr) => msg.pref_source(addr),
                        IpAddr::V6(_) => {
                            return Err(Error::new(
                                io::ErrorKind::InvalidInput,
                                "source hint version must match destination",
                            ))
                        }
                    };
                }

                if let Some(src) = route.source {
                    msg = match src {
                        IpAddr::V4(addr) => msg.source_prefix(addr, route.source_prefix),
                        IpAddr::V6(_) => {
                            return Err(Error::new(
                                io::ErrorKind::InvalidInput,
                                "source version must match destination",
                            ))
                        }
                    };
                }
                msg.execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))
            }
            IpAddr::V6(addr) => {
                let mut msg = route_handle
                    .add()
                    .v6()
                    .table_id(route.table.into())
                    .destination_prefix(addr, route.prefix);

                if let Some(ifindex) = route.ifindex {
                    msg = msg.output_interface(ifindex);
                }

                if let Some(metric) = route.metric {
                    msg = msg.priority(metric);
                }

                if let Some(gateway) = route.gateway {
                    msg = match gateway {
                        IpAddr::V6(addr) => msg.gateway(addr),
                        IpAddr::V4(_) => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "gateway version must match destination",
                            ))
                        }
                    };
                }

                if let Some(src_hint) = route.source_hint {
                    msg = match src_hint {
                        IpAddr::V6(addr) => msg.pref_source(addr),
                        IpAddr::V4(_) => {
                            return Err(Error::new(
                                io::ErrorKind::InvalidInput,
                                "source hint version must match destination",
                            ))
                        }
                    };
                }

                if let Some(src) = route.source {
                    msg = match src {
                        IpAddr::V6(addr) => msg.source_prefix(addr, route.source_prefix),
                        IpAddr::V4(_) => {
                            return Err(Error::new(
                                io::ErrorKind::InvalidInput,
                                "source version must match destination",
                            ))
                        }
                    };
                }
                msg.execute()
                    .await
                    .map_err(|e| Error::new(io::ErrorKind::Other, e.to_string()))
            }
        }
    }

    async fn listen(
        mut messages: UnboundedReceiver<(NetlinkMessage<RouteNetlinkMessage>, SocketAddr)>,
        tx: broadcast::Sender<RouteChange>,
    ) {
        while let Some((message, _)) = messages.next().await {
            if let NetlinkPayload::InnerMessage(msg) = message.payload {
                match msg {
                    RouteNetlinkMessage::NewRoute(msg) => _ = tx.send(RouteChange::Add(msg.into())),
                    RouteNetlinkMessage::DelRoute(msg) => {
                        _ = tx.send(RouteChange::Delete(msg.into()))
                    }
                    _ => (),
                }
            }
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.join_handle.abort();
        self.listen_handle.abort();
    }
}

fn addr_to_ip(addr: RouteAddress) -> Option<IpAddr> {
    match addr {
        RouteAddress::Inet(addr) => Some(addr.into()),
        RouteAddress::Inet6(addr) => Some(addr.into()),
        _ => None,
    }
}

impl From<RouteMessage> for Route {
    fn from(msg: RouteMessage) -> Self {
        let mut gateway = None;
        let mut source = None;
        let mut source_hint = None;
        let mut destination = None;
        let mut ifindex = None;
        let mut metric = None;
        let mut table = msg.header.table as u32;

        for attr in msg.attributes {
            match attr {
                RouteAttribute::Source(addr) => {
                    source = addr_to_ip(addr);
                }
                RouteAttribute::PrefSource(addr) => {
                    source_hint = addr_to_ip(addr);
                }
                RouteAttribute::Destination(addr) => {
                    destination = addr_to_ip(addr);
                }
                RouteAttribute::Gateway(addr) => {
                    gateway = addr_to_ip(addr);
                }
                RouteAttribute::Oif(i) => {
                    ifindex = Some(i);
                }
                RouteAttribute::Priority(priority) => {
                    metric = Some(priority);
                }
                RouteAttribute::Table(real_table) => {
                    table = real_table;
                }
                _ => {}
            }
        }
        // rtnetlink gives None instead of 0.0.0.0 for the default route, but we'll convert to 0 here to make it match the other platforms
        let destination = destination.unwrap_or_else(|| match msg.header.address_family {
            AddressFamily::Inet => Ipv4Addr::UNSPECIFIED.into(),
            AddressFamily::Inet6 => Ipv6Addr::UNSPECIFIED.into(),
            _ => panic!("invalid destination family"),
        });
        Self {
            destination,
            prefix: msg.header.destination_prefix_length,
            source,
            source_prefix: msg.header.source_prefix_length,
            source_hint,
            gateway,
            ifindex,
            table,
            metric,
        }
    }
}

trait RouteExt {
    fn destination_prefix(&self) -> Option<(IpAddr, u8)>;
}

impl RouteExt for RouteMessage {
    fn destination_prefix(&self) -> Option<(IpAddr, u8)> {
        self.attributes
            .iter()
            .flat_map(|attr| {
                if let RouteAttribute::Destination(addr) = attr {
                    addr_to_ip(addr.clone())
                        .map(|addr| (addr, self.header.destination_prefix_length))
                } else {
                    None
                }
            })
            .next()
    }

    // fn if_index(&self) -> Option<u32> {
    //     self.attributes
    //         .iter()
    //         .flat_map(|attr| {
    //             if let RouteAttribute::Oif(index) = attr {
    //                 Some(*index)
    //             } else {
    //                 None
    //             }
    //         })
    //         .next()
    // }
}

#[cfg(test)]
mod tests {
    use netlink_packet_route::IpProtocol;

    use super::*;
    use crate::Rule;

    #[tokio::test]
    async fn test_rule_list() {
        // list all rules on linux
        let handle = Handle::new().unwrap();
        let res = handle.list_rules().await.unwrap();
        for rule in res {
            println!("{:?}", rule);
        }
    }

    #[tokio::test]
    async fn test_rule_add() {
        // list all rules on linux
        let handle = Handle::new().unwrap();
        let mut rule = Rule::default();
        rule.dst = Some(("8.8.8.8".parse().unwrap(), 32));
        rule.table_id = Some(2001);
        // rule.
        let _ = handle.add_rules(vec![rule]).await.unwrap();
    }

    #[tokio::test]
    async fn test_rule_del() {
        // list all rules on linux
        let handle = Handle::new().unwrap();
        let mut rule = Rule::default();
        rule.dst = Some(("8.8.8.8".parse().unwrap(), 32));
        rule.table_id = Some(2001);
        // rule.
        let _ = handle.delete_rules(vec![rule]).await.unwrap();
    }

    #[tokio::test]
    async fn test_rule_add_icmp() {
        // list all rules on linux
        let handle = Handle::new().unwrap();
        let mut rule = Rule::default();
        rule.dst = Some(("8.8.8.8".parse().unwrap(), 32));
        rule.table_id = Some(2001);
        rule.protocol = Some(IpProtocol::Icmp);
        // rule.
        let _ = handle.add_rules(vec![rule]).await.unwrap();
    }
}
