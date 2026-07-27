packer {
  required_plugins {
    tart = {
      source  = "github.com/cirruslabs/tart"
      version = "= 1.21.0"
    }
  }
}

variable "vm_name" {
  type = string
}

variable "cpu_count" {
  type = number
}

variable "memory_gb" {
  type = number
}

variable "disk_size_gb" {
  type = number
}

variable "ssh_public_key" {
  type      = string
  sensitive = true
}

variable "guest_password" {
  type      = string
  sensitive = true
}

source "tart-cli" "gremvm" {
  vm_base_name       = "ghcr.io/cirruslabs/macos-tahoe-vanilla@sha256:e12d678b248f3122e276fa64632970a8e1c6dc60ff6738d21fe9bfa5ea58f426"
  vm_name            = var.vm_name
  cpu_count          = var.cpu_count
  memory_gb          = var.memory_gb
  disk_size_gb       = var.disk_size_gb
  recovery_partition = "relocate"
  headless           = true
  ssh_username       = "admin"
  ssh_password       = "admin"
  ssh_timeout        = "15m"
}

build {
  sources = ["source.tart-cli.gremvm"]

  provisioner "shell" {
    environment_vars = [
      "GREMVM_SSH_PUBLIC_KEY=${var.ssh_public_key}",
      "GREMVM_GUEST_PASSWORD=${var.guest_password}",
    ]
    inline = [
      "install -d -m 0700 /Users/admin/.ssh",
      "printf '%s\\n' \"$GREMVM_SSH_PUBLIC_KEY\" > /Users/admin/.ssh/authorized_keys",
      "chmod 0600 /Users/admin/.ssh/authorized_keys",
      "sudo chown -R admin:staff /Users/admin/.ssh",
      "sudo systemsetup -setremotelogin on",
      "sudo pmset -a sleep 0 disksleep 0 displaysleep 0",
      "sudo sed -i '' -E 's/^[#[:space:]]*PasswordAuthentication[[:space:]].*/PasswordAuthentication no/' /etc/ssh/sshd_config",
      "grep -q '^PasswordAuthentication no$' /etc/ssh/sshd_config || printf '%s\\n' 'PasswordAuthentication no' | sudo tee -a /etc/ssh/sshd_config >/dev/null",
      "sudo sysadminctl -adminUser admin -adminPassword admin -resetPasswordFor admin -newPassword \"$GREMVM_GUEST_PASSWORD\"",
      "printf '%s\\n' \"set-keychain-password -o admin -p $GREMVM_GUEST_PASSWORD /Users/admin/Library/Keychains/login.keychain-db\" | security -i",
      # The source image's auto-login secret is invalid after changing the password.
      "sudo defaults delete /Library/Preferences/com.apple.loginwindow autoLoginUser || true",
      "sudo rm -f /etc/kcpassword",
      "sudo /usr/libexec/ApplicationFirewall/socketfilterfw --setglobalstate off",
    ]
  }
}
