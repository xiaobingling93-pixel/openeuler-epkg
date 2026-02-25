use clap::{Arg, Command};
use color_eyre::Result;
use color_eyre::eyre::eyre;
use std::convert::TryInto;
use std::net::Ipv4Addr;
use std::os::fd::FromRawFd;
use std::str::FromStr;

#[derive(Debug)]
pub struct IfconfigOptions {
    pub interface: String,
    pub address: Option<Ipv4Addr>,
    pub netmask: Option<Ipv4Addr>,
    pub up: bool,
    pub down: bool,
}

pub fn parse_options(matches: &clap::ArgMatches) -> Result<IfconfigOptions> {
    let interface = matches.get_one::<String>("interface")
        .ok_or_else(|| eyre!("ifconfig: missing interface name"))?
        .clone();

    let args: Vec<String> = matches.get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    let mut address = None;
    let mut netmask = None;
    let mut up = false;
    let mut down = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Ok(addr) = Ipv4Addr::from_str(arg) {
            // IPv4 address
            address = Some(addr);
        } else if arg == "netmask" {
            i += 1;
            if i >= args.len() {
                return Err(eyre!("ifconfig: netmask requires an address"));
            }
            netmask = Ipv4Addr::from_str(&args[i]).ok();
        } else if arg == "up" {
            up = true;
        } else if arg == "down" {
            down = true;
        } else {
            // Ignore unknown keywords (e.g., "broadcast")
        }
        i += 1;
    }

    if up && down {
        return Err(eyre!("ifconfig: cannot specify both up and down"));
    }

    Ok(IfconfigOptions {
        interface,
        address,
        netmask,
        up,
        down,
    })
}

pub fn command() -> Command {
    Command::new("ifconfig")
        .about("Configure network interface parameters")
        .arg(Arg::new("interface")
            .required(true)
            .help("Network interface name (e.g., eth0)"))
        .arg(Arg::new("args")
            .help("Address, netmask, up/down keywords")
            .num_args(0..)
            .last(true))
}

fn set_interface_address(interface: &str, addr: Ipv4Addr, netmask: Option<Ipv4Addr>) -> Result<()> {
    // Create a socket for ioctl operations
    use std::os::fd::AsRawFd;
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err(eyre!("socket creation failed: {}", std::io::Error::last_os_error()));
    }
    let sock_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(sock) };

    // Prepare ifreq structure
    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };

    // Copy interface name (null-terminated)
    let ifname_bytes = interface.as_bytes();
    if ifname_bytes.len() >= libc::IFNAMSIZ {
        return Err(eyre!("interface name too long"));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            ifname_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            ifname_bytes.len(),
        );
    }

    // Set IPv4 address
    let addr_in = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(addr).to_be(),
        },
        sin_zero: [0; 8],
    };
    unsafe {
        std::ptr::write_unaligned(
            &mut ifr.ifr_ifru.ifru_addr as *mut libc::sockaddr,
            std::mem::transmute_copy(&addr_in),
        );
    }
    if unsafe { libc::ioctl(sock_fd.as_raw_fd(), libc::SIOCSIFADDR.try_into().unwrap(), &ifr) } < 0 {
        return Err(eyre!("SIOCSIFADDR failed: {}", std::io::Error::last_os_error()));
    }

    // Set netmask if provided
    if let Some(mask) = netmask {
        let mask_in = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: 0,
            sin_addr: libc::in_addr {
                s_addr: u32::from(mask).to_be(),
            },
            sin_zero: [0; 8],
        };
        unsafe {
            std::ptr::write_unaligned(
                &mut ifr.ifr_ifru.ifru_netmask as *mut libc::sockaddr,
                std::mem::transmute_copy(&mask_in),
            );
        }
        if unsafe { libc::ioctl(sock_fd.as_raw_fd(), libc::SIOCSIFNETMASK.try_into().unwrap(), &ifr) } < 0 {
            return Err(eyre!("SIOCSIFNETMASK failed: {}", std::io::Error::last_os_error()));
        }
    }

    Ok(())
}

fn set_interface_flags(interface: &str, up: bool) -> Result<()> {
    use std::os::fd::AsRawFd;
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if sock < 0 {
        return Err(eyre!("socket creation failed: {}", std::io::Error::last_os_error()));
    }
    let sock_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(sock) };

    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    let ifname_bytes = interface.as_bytes();
    if ifname_bytes.len() >= libc::IFNAMSIZ {
        return Err(eyre!("interface name too long"));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            ifname_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            ifname_bytes.len(),
        );
    }

    // Get current flags
    if unsafe { libc::ioctl(sock_fd.as_raw_fd(), libc::SIOCGIFFLAGS.try_into().unwrap(), &ifr) } < 0 {
        return Err(eyre!("SIOCGIFFLAGS failed: {}", std::io::Error::last_os_error()));
    }

    let flags = unsafe { ifr.ifr_ifru.ifru_flags };
    let new_flags = if up {
        flags | libc::IFF_UP as i16
    } else {
        flags & !(libc::IFF_UP as i16)
    };
    ifr.ifr_ifru.ifru_flags = new_flags as _;

    if unsafe { libc::ioctl(sock_fd.as_raw_fd(), libc::SIOCSIFFLAGS.try_into().unwrap(), &ifr) } < 0 {
        return Err(eyre!("SIOCSIFFLAGS failed: {}", std::io::Error::last_os_error()));
    }

    Ok(())
}

pub fn run(options: IfconfigOptions) -> Result<()> {
    if let Some(addr) = options.address {
        set_interface_address(&options.interface, addr, options.netmask)?;
    }

    if options.up {
        set_interface_flags(&options.interface, true)?;
    } else if options.down {
        set_interface_flags(&options.interface, false)?;
    }

    Ok(())
}