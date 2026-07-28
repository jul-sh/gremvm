use strict;
use warnings;

# Match loginwindow's NUL-terminated, 12-byte-block kcpassword format while
# cycling Apple's 11-byte obfuscation key.
my $password = do { local $/; <STDIN> };
my $key = pack "H*", "7d895223d2bcddeaa3b91f";
$password .= "\0";
$password .= "\0" x ((12 - length($password) % 12) % 12);
$password ^= substr(
    $key x ((length($password) + length($key) - 1) / length($key)),
    0,
    length($password)
);
binmode STDOUT;
print $password;
