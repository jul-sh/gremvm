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

variable "guest_user" {
  type = string
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
  display            = "1512x982px"
  recovery_partition = "relocate"
  headless           = true
  ssh_username       = "admin"
  ssh_password       = "admin"
  ssh_timeout        = "15m"
}

build {
  sources = ["source.tart-cli.gremvm"]

  provisioner "file" {
    source      = "${path.root}/password.expect"
    destination = "/tmp/gremvm-password.expect"
  }

  provisioner "shell" {
    use_env_var_file = true
    environment_vars = [
      "GREMVM_GUEST_USER=${var.guest_user}",
      "GREMVM_SSH_PUBLIC_KEY=${var.ssh_public_key}",
      "GREMVM_GUEST_PASSWORD=${var.guest_password}",
    ]
    script = "${path.root}/configure-guest.sh"
  }
}
