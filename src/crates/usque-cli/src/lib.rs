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
                assert!(!args.force);
                assert!(!args.json);
            }
            _ => panic!("expected register"),
        }
    }

    /// The LuCI contract: `usque --config FILE register --accept-tos --force -n NAME`.
    #[test]
    fn parse_register_non_interactive_contract() {
        let cli = Cli::parse_from_args([
            "usque",
            "--config",
            "/etc/usque/config.json",
            "register",
            "--accept-tos",
            "--force",
            "-n",
            "router",
        ])
        .unwrap();
        assert_eq!(cli.config, "/etc/usque/config.json");
        match cli.command {
            crate::cmd::Commands::Register(args) => {
                assert!(args.accept_tos);
                assert!(args.force);
                assert_eq!(args.name.as_deref(), Some("router"));
            }
            _ => panic!("expected register"),
        }
    }

    #[test]
    fn parse_register_json_flag() {
        let cli = Cli::parse_from_args(["usque", "register", "--accept-tos", "--json"]).unwrap();
        match cli.command {
            crate::cmd::Commands::Register(args) => assert!(args.json),
            _ => panic!("expected register"),
        }
    }

    #[test]
    fn parse_enroll_flags() {
        let cli = Cli::parse_from_args(["usque", "enroll", "-n", "phone", "-r", "--json"]).unwrap();
        match cli.command {
            crate::cmd::Commands::Enroll(args) => {
                assert_eq!(args.name.as_deref(), Some("phone"));
                assert!(args.regen_key);
                assert!(args.json);
            }
            _ => panic!("expected enroll"),
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
                assert!(!args.auto_register);
                assert_eq!(args.tunnel.mtu, crate::runtime::DEFAULT_TUNNEL_MTU);
            }
            _ => panic!("expected native-tun"),
        }
    }

    #[test]
    fn parse_nativetun_auto_register_and_tunnel_flags() {
        let cli = Cli::parse_from_args([
            "usque",
            "--config",
            "/tmp/warp.json",
            "native-tun",
            "--auto-register",
            "-n",
            "usque0",
            "-6",
            "-P",
            "8443",
            "-m",
            "1280",
        ])
        .unwrap();
        assert_eq!(cli.config, "/tmp/warp.json");
        match cli.command {
            crate::cmd::Commands::NativeTun(args) => {
                assert!(args.auto_register);
                assert!(args.tunnel.ipv6);
                assert_eq!(args.tunnel.connect_port, 8443);
                assert_eq!(args.tunnel.mtu, 1280);
            }
            _ => panic!("expected native-tun"),
        }
    }

    /// Non-Cloudflare endpoints: `--authority` and `--protocol` must reach the
    /// connect options, and `-s`/`-P` must stay overridable alongside them.
    #[test]
    fn parse_nativetun_custom_endpoint_overrides() {
        let cli = Cli::parse_from_args([
            "usque",
            "--config",
            "/etc/usque/custom.json",
            "native-tun",
            "-n",
            "warp0",
            "-s",
            "masque.example.com",
            "-P",
            "8443",
            "--authority",
            "masque.example.com",
            "--protocol",
            "connect-ip",
        ])
        .unwrap();
        match cli.command {
            crate::cmd::Commands::NativeTun(args) => {
                assert_eq!(args.tunnel.sni_address, "masque.example.com");
                assert_eq!(args.tunnel.connect_port, 8443);
                assert_eq!(args.tunnel.authority, "masque.example.com");
                assert_eq!(args.tunnel.protocol, "connect-ip");
                assert_eq!(
                    crate::runtime::connect_uri_from_authority(&args.tunnel.authority),
                    "https://masque.example.com"
                );
            }
            _ => panic!("expected native-tun"),
        }
    }

    #[test]
    fn custom_endpoint_defaults_are_cloudflare_compatible() {
        let cli = Cli::parse_from_args(["usque", "native-tun"]).unwrap();
        match cli.command {
            crate::cmd::Commands::NativeTun(args) => {
                assert_eq!(args.tunnel.authority, usque_cloudflare_api::CONNECT_URI);
                assert_eq!(args.tunnel.sni_address, usque_cloudflare_api::CONNECT_SNI);
                assert_eq!(args.tunnel.protocol, "cf-connect-ip");
            }
            _ => panic!("expected native-tun"),
        }
    }

    #[test]
    fn removed_proxy_subcommands_are_gone() {
        for removed in ["socks", "http-proxy", "port-fw"] {
            assert!(
                Cli::parse_from_args(["usque", removed]).is_err(),
                "{removed} must no longer be a subcommand"
            );
        }
    }

    #[test]
    fn global_config_flag() {
        let cli = Cli::parse_from_args(["usque", "-c", "/tmp/warp.json", "enroll"]).unwrap();
        assert_eq!(cli.config, "/tmp/warp.json");
    }
}
