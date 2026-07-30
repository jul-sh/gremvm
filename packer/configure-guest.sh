#!/bin/bash
set -euo pipefail

user=${GREMVM_GUEST_USER:?}
password=${GREMVM_GUEST_PASSWORD:?}
unset GREMVM_GUEST_PASSWORD
key=${GREMVM_SSH_PUBLIC_KEY:?}
helper=/tmp/gremvm-password.expect
trap 'unset password; rm -f "$helper"' EXIT

if [[ "$user" == admin ]]; then
    printf %s "$password" | /usr/bin/expect "$helper" reset admin
    printf %s "$password" | /usr/bin/expect "$helper" keychain admin
else
    printf %s "$password" | /usr/bin/expect "$helper" add "$user"
fi

home=/Users/$user
sudo install -m 0600 -o "$user" -g staff /dev/null "$home/.skipbuddy"
sudo install -d -m 0700 -o "$user" -g staff "$home/.ssh"
printf '%s\n' "$key" | sudo tee "$home/.ssh/authorized_keys" >/dev/null
sudo chown "$user":staff "$home/.ssh/authorized_keys"
sudo chmod 0600 "$home/.ssh/authorized_keys"

sudo systemsetup -setremotelogin on
sudo pmset -a sleep 0 disksleep 0 displaysleep 0
sudo sed -i '' -E \
    's/^[#[:space:]]*PasswordAuthentication[[:space:]].*/PasswordAuthentication no/' \
    /etc/ssh/sshd_config
grep -q '^PasswordAuthentication no$' /etc/ssh/sshd_config \
    || printf '%s\n' 'PasswordAuthentication no' \
        | sudo tee -a /etc/ssh/sshd_config >/dev/null

printf %s "$password" | /usr/bin/expect "$helper" autologin "$user"

if [[ "$user" != admin ]]; then
    /usr/bin/id -Gn "$user" | /usr/bin/grep -qw admin
    sudo /usr/sbin/sysadminctl -secureTokenStatus "$user" 2>&1 \
        | /usr/bin/grep -q 'ENABLED'
    bootstrap_password=$(/usr/bin/uuidgen)
    printf %s "$bootstrap_password" | /usr/bin/expect "$helper" reset admin
    unset bootstrap_password
    sudo rm -f /Users/admin/.ssh/authorized_keys
    sudo /usr/bin/pwpolicy -u admin disableuser
    sudo /usr/bin/dscl . -create /Users/admin IsHidden 1
fi

sudo /usr/libexec/ApplicationFirewall/socketfilterfw --setglobalstate off
