{
  imageVersion ? "development",
  lib,
  ...
}:
{
  imports = [ ./modules/grok-guest.nix ];

  grok.guest = {
    enable = true;
    inherit imageVersion;
  };

  networking = {
    hostName = "grok-utility";
    useDHCP = false;
    dhcpcd.enable = false;
    firewall.enable = true;
  };
  systemd.network.enable = false;

  # The production guest communicates through an allowlisted host socket. It
  # has no general-purpose NIC or inbound administration service.
  services.openssh.enable = false;
  security.sudo.enable = false;
  security.polkit.enable = false;

  users = {
    mutableUsers = false;
    allowNoPasswordLogin = true;
    users.root.hashedPassword = "!";
  };

  boot = {
    initrd.availableKernelModules = [
      "hv_vmbus"
      "hv_storvsc"
    ];
    kernelModules = [
      "hv_sock"
      "9p"
      "9pnet"
    ];
  };
  virtualisation.hypervGuest.enable = true;

  documentation = {
    enable = false;
    doc.enable = false;
    info.enable = false;
    man.enable = false;
  };

  system.stateVersion = "25.11";
}
