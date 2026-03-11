use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::convert::TryInto;
use std::net::Ipv4Addr;
use std::os::fd::FromRawFd;
use std::str::FromStr;

#[derive(Debug)]
pub struct RouteOptions {
    pub operation: Operation,
    pub target: Target,
    pub gateway: Option<Ipv4Addr>,
    pub interface: Option<String>,
}

#[derive(Debug)]
pub enum Operation {
    Add,
    Delete,
    // List not implemented yet
}

#[derive(Debug)]
pub enum Target {
    Default,
    Network(Ipv4Addr, Option<Ipv4Addr>), // address, netmask
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<RouteOptions> {
    let operation = match matches.get_one::<String>("operation").map(|s| s.as_str()) {
        Some("add") => Operation::Add,
        Some("del") => Operation::Delete,
        Some(op) => return Err(eyre!("route: unknown operation '{}'", op)),
        None => return Err(eyre!("route: missing operation (add/del)")),
    };

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let mut gateway = None;
    let mut interface = None;
    let mut netmask = None;
    let mut network = None;
    let mut is_default = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "default" {
            is_default = true;
        } else if arg == "-net" {
            i += 1;
            if i >= args.len() {
                return Err(eyre!("route: -net requires a network address"));
            }
            network = Ipv4Addr::from_str(&args[i]).ok();
        } else if arg == "gw" || arg == "gateway" {
            i += 1;
            if i >= args.len() {
                return Err(eyre!("route: gw requires an address"));
            }
            gateway = Ipv4Addr::from_str(&args[i]).ok();
        } else if arg == "netmask" {
            i += 1;
            if i >= args.len() {
                return Err(eyre!("route: netmask requires a mask"));
            }
            netmask = Ipv4Addr::from_str(&args[i]).ok();
        } else if arg == "dev" {
            i += 1;
            if i >= args.len() {
                return Err(eyre!("route: dev requires an interface name"));
            }
            interface = Some(args[i].clone());
        } else if let Ok(addr) = Ipv4Addr::from_str(arg) {
            // Bare IP address: could be network or gateway
            // Heuristic: if we haven't seen a network yet, treat as network
            if network.is_none() {
                network = Some(addr);
            } else {
                // Otherwise treat as gateway (simplistic)
                gateway = Some(addr);
            }
        } else {
            // Ignore unknown
        }
        i += 1;
    }

    let target = if is_default {
        Target::Default
    } else if let Some(net) = network {
        Target::Network(net, netmask)
    } else {
        Target::Default // fallback
    };

    Ok(RouteOptions {
        operation,
        target,
        gateway,
        interface,
    })
}

pub fn command() -> Command {
    Command::new("route")
        .about("Manipulate the IP routing table")
        .arg(Arg::new("operation")
            .required(true)
            .value_parser(["add", "del"])
            .help("Operation: add or del"))
        .arg(Arg::new("args")
            .help("Route specification (default, -net, gw, netmask, dev)")
            .num_args(0..)
            .last(true))
}

fn add_route(target: &Target, gateway: Option<Ipv4Addr>, interface: Option<&str>) -> Result<()> {
    use std::os::fd::AsRawFd;
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err(eyre!("socket creation failed: {}", std::io::Error::last_os_error()));
    }
    let sock_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(sock) };

    let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };

    // Set destination
    let (dest_addr, dest_mask) = match target {
        Target::Default => (0u32.to_be(), 0u32.to_be()),
        Target::Network(addr, mask) => {
            let mask_val = mask.map(|m| u32::from(m).to_be()).unwrap_or(0xFFFFFFFFu32.to_be());
            (u32::from(*addr).to_be(), mask_val)
        }
    };

    let dest_sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr { s_addr: dest_addr },
        sin_zero: [0; 8],
    };
    unsafe {
        std::ptr::write_unaligned(
            &mut rt.rt_dst as *mut _ as *mut libc::sockaddr,
            std::mem::transmute_copy(&dest_sin),
        );
    }

    // Set gateway if provided
    if let Some(gw) = gateway {
        let gw_sin = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: 0,
            sin_addr: libc::in_addr { s_addr: u32::from(gw).to_be() },
            sin_zero: [0; 8],
        };
        unsafe {
            std::ptr::write_unaligned(
                &mut rt.rt_gateway as *mut _ as *mut libc::sockaddr,
                std::mem::transmute_copy(&gw_sin),
            );
        }
        rt.rt_flags |= libc::RTF_GATEWAY as u16;
    }

    // Set interface if provided. rt_dev is a pointer; we must point it at a valid buffer.
    let mut rt_dev_buf = [0u8; libc::IFNAMSIZ];
    if let Some(iface) = interface {
        let ifname_bytes = iface.as_bytes();
        if ifname_bytes.len() >= libc::IFNAMSIZ {
            log::debug!("route: interface name too long: {}", iface);
            return Err(eyre!("interface name too long"));
        }
        rt_dev_buf[..ifname_bytes.len()].copy_from_slice(ifname_bytes);
        rt.rt_dev = rt_dev_buf.as_mut_ptr() as *mut libc::c_char;
        rt.rt_flags |= libc::RTF_UP as u16;
    }

    // Set mask
    let mask_sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr { s_addr: dest_mask },
        sin_zero: [0; 8],
    };
    unsafe {
        std::ptr::write_unaligned(
            &mut rt.rt_genmask as *mut _ as *mut libc::sockaddr,
            std::mem::transmute_copy(&mask_sin),
        );
    }

    // Set remaining fields
    rt.rt_flags |= libc::RTF_UP as u16;
    if matches!(target, Target::Default) {
        rt.rt_flags |= libc::RTF_DEFAULT as u16;
    }

    if unsafe { libc::ioctl(sock_fd.as_raw_fd(), libc::SIOCADDRT.try_into().unwrap(), &rt) } < 0 {
        let e = std::io::Error::last_os_error();
        log::debug!("route: SIOCADDRT failed: {} (target={:?} gw={:?} dev={:?})", e, target, gateway, interface);
        return Err(eyre!("SIOCADDRT failed: {}", e));
    }

    Ok(())
}

pub fn run(options: RouteOptions) -> Result<()> {
    log::debug!("route: {:?} {:?} gw={:?} dev={:?}", options.operation, options.target, options.gateway, options.interface);
    match options.operation {
        Operation::Add => add_route(&options.target, options.gateway, options.interface.as_deref()),
        Operation::Delete => {
            log::debug!("route: delete not yet implemented (target={:?} gw={:?} dev={:?})", options.target, options.gateway, options.interface);
            Err(eyre!("route delete not yet implemented"))
        }
    }
}