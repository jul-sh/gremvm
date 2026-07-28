#!/bin/bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: guest-setup.sh <base64-ssh-public-key> <base64-password>" >&2
  exit 2
fi

key_file=$(/usr/bin/mktemp -t gremvm-key)
kcpassword_file=$(/usr/bin/mktemp -t gremvm-kcpassword)
cleanup() {
  /bin/rm -f "$key_file" "$kcpassword_file"
}
trap cleanup EXIT HUP INT TERM

printf '%s' "$1" | /usr/bin/base64 -D >"$key_file"
/usr/bin/ssh-keygen -l -f "$key_file" >/dev/null
admin_password=$(printf '%s' "$2" | /usr/bin/base64 -D)
if [[ ! $admin_password =~ ^[[:xdigit:]]{48}$ ]]; then
  echo "invalid guest password" >&2
  exit 2
fi

# Lume's image starts with lume/lume. Recovery can rerun this as admin.
case $(/usr/bin/id -un) in
  lume) sudo_password=lume ;;
  admin) sudo_password=$admin_password ;;
  *) echo "guest setup must run as lume or admin" >&2; exit 2 ;;
esac
printf '%s\n' "$sudo_password" | /usr/bin/sudo -S -p '' -v
unset sudo_password

if /usr/bin/id admin >/dev/null 2>&1; then
  /usr/bin/sudo -n /usr/sbin/sysadminctl \
    -resetPasswordFor admin -newPassword "$admin_password"
else
  /usr/bin/sudo -n /usr/sbin/sysadminctl \
    -addUser admin -fullName "GremVM Admin" -password "$admin_password" -admin
fi

/usr/bin/sudo -n /usr/bin/install -d -m 0700 -o admin -g staff /Users/admin/.ssh
/usr/bin/sudo -n /usr/bin/install -m 0600 -o admin -g staff \
  "$key_file" /Users/admin/.ssh/authorized_keys

printf '%s\n' \
  'PasswordAuthentication no' \
  'KbdInteractiveAuthentication no' \
  'ChallengeResponseAuthentication no' \
  'PubkeyAuthentication yes' \
  'GatewayPorts clientspecified' \
  'AllowTcpForwarding remote' >"$key_file"
/usr/bin/sudo -n /usr/bin/install -d -m 0755 -o root -g wheel /etc/ssh/sshd_config.d
/usr/bin/sudo -n /usr/bin/install -m 0644 -o root -g wheel \
  "$key_file" /etc/ssh/sshd_config.d/gremvm.conf
/usr/bin/sudo -n /usr/sbin/sshd -t

preferences=/Users/admin/Library/Preferences
/usr/bin/sudo -n /usr/bin/install -d -m 0755 -o admin -g staff "$preferences"
for setting in DidSeeCloudSetup DidSeePrivacy DidSeeSiriSetup DidSeeTouchIDSetup DidSeeTrueToneSetup; do
  /usr/bin/sudo -n -u admin /usr/bin/defaults write \
    "$preferences/com.apple.SetupAssistant" "$setting" -bool true
done
/usr/bin/sudo -n -u admin /usr/bin/defaults write \
  "$preferences/com.apple.SetupAssistant" GestureMovieSeen -string none
/usr/bin/sudo -n -u admin /usr/bin/defaults write \
  "$preferences/com.apple.SetupAssistant" LastSeenBuddyBuildVersion -string 25G72
/usr/bin/sudo -n -u admin /usr/bin/defaults write \
  "$preferences/com.apple.SetupAssistant" LastSeenCloudProductVersion -string 26.6
/usr/bin/sudo -n /usr/bin/defaults write \
  /Library/Preferences/com.apple.SetupAssistant LastSeenBuddyBuildVersion -string 25G72
/usr/bin/sudo -n /usr/bin/defaults write \
  /Library/Preferences/com.apple.SetupAssistant LastSeenCloudProductVersion -string 26.6
/usr/bin/sudo -n -u admin /usr/bin/defaults write \
  "$preferences/.GlobalPreferences" AppleKeyboardUIMode -int 3
for setting in askForPassword askForPasswordDelay idleTime; do
  /usr/bin/sudo -n -u admin /usr/bin/defaults write \
    "$preferences/com.apple.screensaver" "$setting" -int 0
done

printf '%s' "$admin_password" | /usr/bin/perl -e '
  use strict;
  my $password = do { local $/; <STDIN> } . "\0";
  my $key = pack "H*", "7d895223d2bcddeaa3b91f";
  $password .= "\0" x ((12 - length($password) % 12) % 12);
  $password ^= substr($key x int((length($password) + length($key) - 1) / length($key)), 0, length($password));
  binmode STDOUT;
  print $password;
' >"$kcpassword_file"
/usr/bin/sudo -n /usr/bin/install -m 0600 -o root -g wheel \
  "$kcpassword_file" /etc/kcpassword
/usr/bin/sudo -n /usr/bin/defaults write \
  /Library/Preferences/com.apple.loginwindow autoLoginUser admin
/usr/bin/sudo -n /usr/bin/defaults write \
  /Library/Preferences/com.apple.loginwindow lastUser -string loggedIn
/usr/bin/sudo -n /usr/bin/defaults write \
  /Library/Preferences/com.apple.loginwindow lastUserName -string admin
/usr/bin/sudo -n /usr/bin/defaults write \
  /Library/Preferences/com.apple.loginwindow autoLoginUserScreenLocked -bool false

/usr/bin/sudo -n /usr/bin/pmset -a sleep 0 disksleep 0 displaysleep 0
/usr/bin/sudo -n /usr/libexec/ApplicationFirewall/socketfilterfw --setglobalstate off

# Remove the image's public bootstrap credential only after setup is complete.
bootstrap_password=$(/usr/bin/openssl rand -hex 24)
/usr/bin/sudo -n /usr/sbin/sysadminctl \
  -resetPasswordFor lume -newPassword "$bootstrap_password"
/usr/bin/sudo -n /usr/bin/install -m 0444 -o root -g wheel \
  /dev/null /var/db/gremvm-ready
