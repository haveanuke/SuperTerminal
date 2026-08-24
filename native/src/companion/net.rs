//! Tailnet address discovery: the server binds ONLY to an address in the
//! CGNAT range Tailscale assigns (100.64.0.0/10). Selection is a pure
//! function over interface candidates so it is testable without a tailnet;
//! the collector wraps getifaddrs in the repo's existing extern-C style.

use std::net::Ipv4Addr;

#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub addr: Ipv4Addr,
    pub up: bool,
}

/// Pick the tailnet address to bind: an UP interface address inside
/// 100.64.0.0/10. Multiple candidates resolve deterministically (lowest
/// address) so restarts bind consistently.
pub fn pick_tailnet(candidates: &[Candidate]) -> Option<Ipv4Addr> {
    candidates
        .iter()
        .filter(|candidate| candidate.up)
        .map(|candidate| candidate.addr)
        .filter(|addr| {
            let octets = addr.octets();
            octets[0] == 100 && (64..=127).contains(&octets[1])
        })
        .min()
}

/// Enumerate live IPv4 interface addresses and pick the tailnet one.
pub fn tailnet_ipv4() -> Option<Ipv4Addr> {
    pick_tailnet(&interface_candidates())
}

const IFF_UP: u32 = 0x1;
const AF_INET: u8 = 2;

#[repr(C)]
struct Ifaddrs {
    ifa_next: *mut Ifaddrs,
    ifa_name: *mut std::os::raw::c_char,
    ifa_flags: u32,
    ifa_addr: *mut SockaddrIn,
    ifa_netmask: *mut SockaddrIn,
    ifa_dstaddr: *mut SockaddrIn,
    ifa_data: *mut std::os::raw::c_void,
}

/// Only the prefix we read (sa_family + the in_addr for AF_INET).
#[repr(C)]
struct SockaddrIn {
    sin_len: u8,
    sin_family: u8,
    sin_port: u16,
    sin_addr: [u8; 4],
    sin_zero: [u8; 8],
}

extern "C" {
    fn getifaddrs(ifap: *mut *mut Ifaddrs) -> i32;
    fn freeifaddrs(ifa: *mut Ifaddrs);
}

fn interface_candidates() -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut list: *mut Ifaddrs = std::ptr::null_mut();
    // SAFETY: standard getifaddrs contract — freed with freeifaddrs below;
    // every pointer is checked before dereference.
    unsafe {
        if getifaddrs(&mut list) != 0 {
            return out;
        }
        let mut cursor = list;
        while !cursor.is_null() {
            let entry = &*cursor;
            if !entry.ifa_addr.is_null() && (*entry.ifa_addr).sin_family == AF_INET {
                let a = (*entry.ifa_addr).sin_addr;
                out.push(Candidate {
                    addr: Ipv4Addr::new(a[0], a[1], a[2], a[3]),
                    up: entry.ifa_flags & IFF_UP != 0,
                });
            }
            cursor = entry.ifa_next;
        }
        freeifaddrs(list);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(addr: [u8; 4], up: bool) -> Candidate {
        Candidate {
            addr: Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]),
            up,
        }
    }

    #[test]
    fn up_cgnat_address_wins() {
        let picked = pick_tailnet(&[c([192, 168, 1, 10], true), c([100, 101, 5, 9], true)]);
        assert_eq!(picked, Some(Ipv4Addr::new(100, 101, 5, 9)));
    }

    #[test]
    fn down_interfaces_are_skipped() {
        assert_eq!(pick_tailnet(&[c([100, 101, 5, 9], false)]), None);
    }

    #[test]
    fn non_cgnat_never_chosen() {
        // 100.128.x is OUTSIDE 100.64.0.0/10 (valid second octet: 64..=127).
        let picked = pick_tailnet(&[
            c([192, 168, 1, 10], true),
            c([100, 128, 0, 1], true),
            c([100, 63, 255, 255], true),
            c([10, 0, 0, 5], true),
        ]);
        assert_eq!(picked, None);
    }

    #[test]
    fn range_boundaries_are_inclusive() {
        assert_eq!(
            pick_tailnet(&[c([100, 64, 0, 1], true)]),
            Some(Ipv4Addr::new(100, 64, 0, 1))
        );
        assert_eq!(
            pick_tailnet(&[c([100, 127, 255, 254], true)]),
            Some(Ipv4Addr::new(100, 127, 255, 254))
        );
    }

    #[test]
    fn multiple_candidates_pick_lowest_deterministically() {
        let picked = pick_tailnet(&[c([100, 100, 2, 2], true), c([100, 64, 9, 9], true)]);
        assert_eq!(picked, Some(Ipv4Addr::new(100, 64, 9, 9)));
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(pick_tailnet(&[]), None);
    }
}
