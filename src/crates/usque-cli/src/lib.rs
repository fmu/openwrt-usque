pub mod cmd;
pub mod runtime;

use clap::Parser;

#[derive(Parser)]
#[command(name = "usque", about = "Cloudflare WARP MASQUE client (Rust)")]
pub struct Cli {
    #[arg(short, long, default_value = "config.json", global = true)]
    pub config: String,

    #[command(subcommand)]
    pub command: cmd::Commands,
}

impl Cli {
    pub fn parse_from_args<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Self::try_parse_from(args)
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    /// Catches duplicate short flags and other clap misconfigurations at test time.
    #[test]
    fn cli_debug_assert_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parse_top_level_help() {
        assert!(Cli::parse_from_args(["usque", "--help"]).is_err());
    }

    #[test]
    fn parse_register_flags() {
        let cli = Cli::parse_from_args([
            "usque", "register", "-a", "-l", "en_US", "-m", "PC", "-n", "dev",
        ])
        .unwrap();
        match cli.command {
            crate::cmd::Commands::Register(args) => {
                assert!(args.accept_tos);
                assert_eq!(args.locale, "en_US");
                assert_eq!(args.model, "PC");
                assert_eq!(args.name.as_deref(), Some("dev"));
            }
            _ => panic!("expected register"),
        }
    }

    #[test]
    fn parse_enroll_flags() {
        let cli = Cli::parse_from_args(["usque", "enroll", "-n", "phone", "-r"]).unwrap();
        match cli.command {
            crate::cmd::Commands::Enroll(args) => {
                assert_eq!(args.name.as_deref(), Some("phone"));
                assert!(args.regen_key);
            }
            _ => panic!("expected enroll"),
        }
    }

    #[test]
    fn parse_socks_flags() {
        let cli = Cli::parse_from_args([
            "usque",
            "socks",
            "--auto-register",
            "-b",
            "127.0.0.1",
            "-p",
            "1080",
            "-u",
            "user",
            "-w",
            "pass",
            "-d",
            "1.1.1.1",
            "-t",
            "3s",
            "-l",
            "-6",
            "-P",
            "8443",
        ])
        .unwrap();
        match cli.command {
            crate::cmd::Commands::Socks(args) => {
                assert!(args.auto_register);
                assert_eq!(args.bind, "127.0.0.1");
                assert_eq!(args.port, 1080);
                assert_eq!(args.username.as_deref(), Some("user"));
                assert_eq!(args.password.as_deref(), Some("pass"));
                assert_eq!(args.dns, vec!["1.1.1.1"]);
                assert!(args.local_dns);
                assert!(args.tunnel.ipv6);
                assert_eq!(args.tunnel.connect_port, 8443);
            }
            _ => panic!("expected socks"),
        }
    }

    #[test]
    fn parse_http_proxy_flags() {
        let cli = Cli::parse_from_args([
            "usque",
            "http-proxy",
            "--auto-register",
            "-p",
            "8080",
            "-w",
            "secret",
        ])
        .unwrap();
        match cli.command {
            crate::cmd::Commands::HttpProxy(args) => {
                assert!(args.auto_register);
                assert_eq!(args.port, 8080);
                assert_eq!(args.password.as_deref(), Some("secret"));
            }
            _ => panic!("expected http-proxy"),
        }
    }

    #[test]
    fn parse_portfw_flags() {
        let cli = Cli::parse_from_args([
            "usque",
            "port-fw",
            "-L",
            "8080:127.0.0.1:80",
            "-R",
            "9090:example.com:443",
        ])
        .unwrap();
        match cli.command {
            crate::cmd::Commands::PortFw(args) => {
                assert_eq!(args.local_ports, vec!["8080:127.0.0.1:80"]);
                assert_eq!(args.remote_ports, vec!["9090:example.com:443"]);
            }
            _ => panic!("expected port-fw"),
        }
    }

    #[test]
    fn parse_nativetun_flags() {
        let cli = Cli::parse_from_args(["usque", "native-tun", "-n", "usque0", "-I", "--persist"])
            .unwrap();
        match cli.command {
            crate::cmd::Commands::NativeTun(args) => {
                assert_eq!(args.interface_name, "usque0");
                assert!(args.no_iproute2);
                assert!(args.persist);
            }
            _ => panic!("expected native-tun"),
        }
    }

    #[test]
    fn global_config_flag() {
        let cli = Cli::parse_from_args(["usque", "-c", "/tmp/warp.json", "enroll"]).unwrap();
        assert_eq!(cli.config, "/tmp/warp.json");
    }
}
