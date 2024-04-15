use std::net::{IpAddr, Ipv4Addr};

use net_route::{Handle, Rule};
use netlink_packet_route::rule::RuleAttribute;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // list all rules on linux
    #[cfg(target_os = "linux")]
    {
        let handle = Handle::new().unwrap();
        let mut rule = Rule::default();
        rule.dst = Some("8.8.8.8".parse().unwrap());
        rule.table_id = Some(2001);
        let _ = handle.add_rules(vec![rule.clone()]).await.unwrap();

        let rules = handle.list_rules().await.unwrap();
        rules.iter().for_each(|x| {
            println!("{:?}", x);
        });
        assert!(rules
            .iter()
            .find(|rule| {
                rule.attributes
                    .contains(&RuleAttribute::Destination(IpAddr::V4(Ipv4Addr::new(
                        8, 8, 8, 8,
                    ))))
                    && rule.attributes.contains(&RuleAttribute::Table(2001))
            })
            .is_some(),);
        handle.delete_rules(vec![rule.clone()]).await.unwrap();
    }
    Ok(())
}
