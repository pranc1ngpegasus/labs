{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.codex-proxy;
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    types
    ;

  argumentList = [
    "${cfg.package}/bin/codex-proxy"
    "--host"
    cfg.host
    "--port"
    (toString cfg.port)
    "--auth-file"
    cfg.authFile
    "--token-url"
    cfg.tokenUrl
    "--backend"
    cfg.backend
  ];

  command = pkgs.writeShellScript "codex-proxy-service" ''
    set -eu
    ${lib.optionalString (cfg.apiKeyFile != null) ''
      export CODEX_PROXY_API_KEY="$(cat -- ${lib.escapeShellArg cfg.apiKeyFile})"
    ''}
    exec ${lib.escapeShellArgs argumentList}
  '';
in
{
  options.services.codex-proxy = {
    enable = mkEnableOption "the Codex OAuth proxy";

    package = mkOption {
      type = types.package;
      description = "The codex-proxy package to run.";
    };

    host = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = "Address on which codex-proxy listens.";
    };

    port = mkOption {
      type = types.port;
      default = 8080;
      description = "TCP port on which codex-proxy listens.";
    };

    authFile = mkOption {
      type = types.path;
      default = "${config.home.homeDirectory}/.codex/auth.json";
      description = "Codex OAuth auth.json path.";
    };

    tokenUrl = mkOption {
      type = types.str;
      default = "https://auth.openai.com/oauth/token";
      description = "OAuth token endpoint.";
    };

    backend = mkOption {
      type = types.str;
      default = "https://chatgpt.com/backend-api/wham";
      description = "ChatGPT backend base URL.";
    };

    apiKeyFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = ''
        File containing the client API key. It is read at service startup so
        the secret is not embedded in the Nix store or service definition.
      '';
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.apiKeyFile != null;
        message = "services.codex-proxy.apiKeyFile must be set for a stable service API key";
      }
    ];

    launchd.agents.codex-proxy = mkIf pkgs.stdenv.isDarwin {
      config = {
        ProgramArguments = [ command ];
        RunAtLoad = true;
        KeepAlive = true;
        ProcessType = "Background";
        StandardOutPath = "${config.home.homeDirectory}/Library/Logs/codex-proxy.log";
        StandardErrorPath = "${config.home.homeDirectory}/Library/Logs/codex-proxy.error.log";
      };
    };

    systemd.user.services.codex-proxy = mkIf pkgs.stdenv.isLinux {
      Unit = {
        Description = "Codex OAuth proxy";
        After = [ "network-online.target" ];
      };
      Service = {
        ExecStart = command;
        Restart = "on-failure";
        RestartSec = 5;
      };
      Install.WantedBy = [ "default.target" ];
    };
  };
}
