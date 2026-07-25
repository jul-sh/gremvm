#!/bin/sh
set -eu
umask 077

program=${0##*/}
repo=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
api_secret=$repo/secrets/CLOUDFLARE_API_TOKEN.age
tunnel_secret=$repo/secrets/CLOUDFLARE_TUNNEL_CREDENTIALS.age
api=https://api.cloudflare.com/client/v4
zone=eviljuliette.com
hostname=gremvm.eviljuliette.com
tunnel_name=gremvm
app_name='GremVM SSH'
policy_name='GremVM owner'
access_email=${GREMVM_CLOUDFLARE_ACCESS_EMAIL:-}

die() {
    echo "$program: $*" >&2
    exit 1
}
need() { command -v "$1" > /dev/null || die "missing dependency '$1'; run through nix develop"; }

mode=${1:-}
if [ "$#" -ne 1 ] || { [ "$mode" != check ] && [ "$mode" != apply ]; }; then
    die "usage: GREMVM_CLOUDFLARE_ACCESS_EMAIL=you@example.com $program check|apply"
fi
for dependency in curl jq keytap; do need "$dependency"; done
printf '%s' "$access_email" | jq -eR 'test("^[^[:space:]@]+@[^[:space:]@]+\\.[^[:space:]@]+$")' > /dev/null ||
    die "set GREMVM_CLOUDFLARE_ACCESS_EMAIL to the one identity allowed through Access"
[ -r "$api_secret" ] || die "missing encrypted API token: $api_secret"
[ "$(LC_ALL=C /usr/bin/grep -a -c '^-> ' "$api_secret")" -eq 1 ] || die "API token must have exactly one Keytap recipient"

api_token=$(keytap decrypt keytap < "$api_secret")
[ -n "$api_token" ] || die "Cloudflare API token decrypted to an empty value"
trap 'unset api_token credentials response tunnel_material' EXIT HUP INT TERM

cf() {
    method=$1
    path=$2
    if [ "$#" -eq 3 ]; then
        response=$(printf 'Authorization: Bearer %s\n' "$api_token" | curl --silent --show-error \
            --connect-timeout 15 --max-time 60 --proto '=https' --tlsv1.2 \
            --header @- --header 'Content-Type: application/json' --request "$method" \
            --data "$3" "$api$path") || return
    else
        response=$(printf 'Authorization: Bearer %s\n' "$api_token" | curl --silent --show-error \
            --connect-timeout 15 --max-time 60 --proto '=https' --tlsv1.2 \
            --header @- --request "$method" "$api$path") || return
    fi
    if ! printf '%s' "$response" | jq -e '.success == true' > /dev/null 2>&1; then
        printf '%s' "$response" | jq -r '.errors[]? | "Cloudflare API \(.code): \(.message)"' >&2 || true
        return 1
    fi
    printf '%s\n' "$response"
}

store_tunnel_material() (
    secret_tmp=$(/usr/bin/mktemp "$repo/secrets/.cloudflare-tunnel.XXXXXX")
    trap '/bin/rm -f "$secret_tmp"' EXIT HUP INT TERM
    printf '%s' "$tunnel_material" | keytap encrypt keytap > "$secret_tmp"
    [ "$(LC_ALL=C /usr/bin/grep -a -c '^-> ' "$secret_tmp")" -eq 1 ] ||
        die "refusing tunnel credentials with anything other than one Keytap recipient"
    /bin/chmod 600 "$secret_tmp"
    /bin/mv -f "$secret_tmp" "$tunnel_secret"
    trap - EXIT HUP INT TERM
)

verify=$(cf GET /user/tokens/verify) || die "token verification failed before mutation"
[ "$(printf '%s' "$verify" | jq -r '.result.status')" = active ] || die "Cloudflare API token is not active"
zones=$(cf GET "/zones?name=$zone&status=active&per_page=50") || die "zone preflight failed; grant Zone Read for $zone"
[ "$(printf '%s' "$zones" | jq --arg zone "$zone" '[.result[] | select(.name == $zone)] | length')" -eq 1 ] || die "expected exactly one active $zone zone"
account_id=$(printf '%s' "$zones" | jq -r --arg zone "$zone" '.result[] | select(.name == $zone) | .account.id')
zone_id=$(printf '%s' "$zones" | jq -r --arg zone "$zone" '.result[] | select(.name == $zone) | .id')

tunnels=$(cf GET "/accounts/$account_id/cfd_tunnel?name=$tunnel_name&is_deleted=false&per_page=100") ||
    die "Tunnel preflight failed; grant account Cloudflare Tunnel Write"
tunnel_count=$(printf '%s' "$tunnels" | jq --arg name "$tunnel_name" '[.result[] | select(.name == $name and .deleted_at == null)] | length')
[ "$tunnel_count" -le 1 ] || die "duplicate active tunnels named $tunnel_name"
listed_tunnel_id=$(printf '%s' "$tunnels" | jq -r --arg name "$tunnel_name" '.result[] | select(.name == $name and .deleted_at == null) | .id' | /usr/bin/sed -n '1p')
if [ "$tunnel_count" -eq 1 ]; then
    config_source=$(printf '%s' "$tunnels" | jq -r --arg name "$tunnel_name" '.result[] | select(.name == $name and .deleted_at == null) | .config_src')
    [ "$config_source" = local ] || die "same-named tunnel is not locally managed"
fi

credential_state=absent
stored_tunnel_id=
if [ -f "$tunnel_secret" ]; then
    [ "$(LC_ALL=C /usr/bin/grep -a -c '^-> ' "$tunnel_secret")" -eq 1 ] || die "tunnel credentials must have exactly one Keytap recipient"
    credentials=$(keytap decrypt keytap < "$tunnel_secret")
    tunnel_material=$(printf '%s' "$credentials" | jq -ce --arg account "$account_id" --arg name "$tunnel_name" '
        select(type == "object") |
        select(.AccountTag == $account and .TunnelName == $name) |
        select((.TunnelID == null) or (.TunnelID | type == "string" and test("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"))) |
        select(.TunnelSecret | type == "string" and test("^[A-Za-z0-9+/]{43}=$")) |
        {AccountTag, TunnelID, TunnelName, TunnelSecret}
    ') || die "invalid stored tunnel credentials"
    unset credentials
    stored_tunnel_id=$(printf '%s' "$tunnel_material" | jq -r '.TunnelID // empty')
    if [ -n "$stored_tunnel_id" ]; then credential_state=ready; else credential_state=pending; fi
fi
if [ "$tunnel_count" -eq 1 ]; then
    [ "$credential_state" != absent ] || die "tunnel exists but its unrecoverable credential is absent; refusing to replace it"
    [ "$credential_state" = pending ] || [ "$stored_tunnel_id" = "$listed_tunnel_id" ] || die "stored credentials do not match the Cloudflare tunnel"
elif [ "$credential_state" = ready ]; then
    die "stored credentials exist but their Cloudflare tunnel is absent"
fi

records=$(cf GET "/zones/$zone_id/dns_records?name=$hostname&per_page=100") || die "DNS preflight failed; grant DNS Write for $zone"
record_count=$(printf '%s' "$records" | jq --arg host "$hostname" '[.result[] | select(.name == $host)] | length')
[ "$record_count" -le 1 ] || die "multiple DNS records occupy $hostname"
if [ "$record_count" -eq 1 ]; then
    [ "$tunnel_count" -eq 1 ] || die "an unmanaged DNS record already occupies $hostname"
    expected_target=$listed_tunnel_id.cfargotunnel.com
    printf '%s' "$records" | jq -e --arg host "$hostname" --arg target "$expected_target" \
        '.result[0] | .name == $host and .type == "CNAME" and .content == $target and .proxied == true' > /dev/null ||
        die "the existing $hostname record is not the managed Tunnel CNAME"
fi

apps=$(cf GET "/accounts/$account_id/access/apps?per_page=100") || die "Access preflight failed; grant Access: Apps and Policies Write"
app_count=$(printf '%s' "$apps" | jq --arg name "$app_name" --arg host "$hostname" '[.result[] | select(.name == $name or .domain == $host)] | length')
[ "$app_count" -le 1 ] || die "multiple Access applications collide with $app_name or $hostname"
if [ "$app_count" -eq 1 ]; then
    app=$(printf '%s' "$apps" | jq -c --arg name "$app_name" --arg host "$hostname" '.result[] | select(.name == $name or .domain == $host)')
    printf '%s' "$app" | jq -e --arg name "$app_name" --arg host "$hostname" \
        '.name == $name and .domain == $host and .type == "self_hosted"' > /dev/null || die "the existing Access application is not managed by GremVM"
    app_id=$(printf '%s' "$app" | jq -r '.id')
    policies=$(cf GET "/accounts/$account_id/access/apps/$app_id/policies") || die "Access policy preflight failed"
    printf '%s' "$policies" | jq -e --arg name "$policy_name" --arg email "$access_email" '
        [.result[] | select(.name == $name)] as $managed |
        ($managed | length) == 1 and
        ($managed[0].decision == "allow") and
        (($managed[0].include // []) == [{"email":{"email":$email}}]) and
        (($managed[0].exclude // []) == []) and
        (($managed[0].require // []) == [])
    ' > /dev/null || die "the existing Access policy does not exclusively allow $access_email"
fi

echo "Cloudflare preflight passed for $hostname."
if [ "$mode" = check ]; then
    if [ "$credential_state" = pending ]; then
        echo "Tunnel: credential finalization pending"
    elif [ "$tunnel_count" -eq 1 ]; then
        echo "Tunnel: managed"
    else
        echo "Tunnel: will be created"
    fi
    [ "$record_count" -eq 1 ] && echo "DNS: managed" || echo "DNS: will be created"
    [ "$app_count" -eq 1 ] && echo "Access: managed for $access_email" || echo "Access: will be created for $access_email"
    exit 0
fi

need openssl
if [ "$tunnel_count" -eq 0 ]; then
    if [ "$credential_state" = pending ]; then
        generated_secret=$(printf '%s' "$tunnel_material" | jq -r '.TunnelSecret')
    else
        generated_secret=$(openssl rand -base64 32 | /usr/bin/tr -d '\n')
        printf '%s' "$generated_secret" | jq -eR 'test("^[A-Za-z0-9+/]{43}=$")' > /dev/null || die "failed to generate a 32-byte tunnel secret"
        tunnel_material=$(jq -cn --arg account "$account_id" --arg name "$tunnel_name" --arg secret "$generated_secret" \
            '{AccountTag:$account,TunnelID:null,TunnelName:$name,TunnelSecret:$secret}')
        store_tunnel_material
        credential_state=pending
    fi
    body=$(jq -cn --arg name "$tunnel_name" --arg secret "$generated_secret" \
        '{name:$name,config_src:"local",tunnel_secret:$secret}')
    created=$(cf POST "/accounts/$account_id/cfd_tunnel" "$body") || die "failed to create the Cloudflare Tunnel"
    listed_tunnel_id=$(printf '%s' "$created" | jq -er '.result.id | select(type == "string" and length == 36)') || die "Cloudflare returned no tunnel ID"
    tunnel_material=$(printf '%s' "$tunnel_material" | jq -c --arg id "$listed_tunnel_id" '.TunnelID = $id')
    unset generated_secret
    store_tunnel_material
    credential_state=ready
elif [ "$credential_state" = pending ]; then
    tunnel_material=$(printf '%s' "$tunnel_material" | jq -c --arg id "$listed_tunnel_id" '.TunnelID = $id')
    store_tunnel_material
    credential_state=ready
fi

if [ "$record_count" -eq 0 ]; then
    target=$listed_tunnel_id.cfargotunnel.com
    body=$(jq -cn --arg name "$hostname" --arg content "$target" \
        '{type:"CNAME",name:$name,content:$content,proxied:true,ttl:1}')
    cf POST "/zones/$zone_id/dns_records" "$body" > /dev/null || die "failed to create the Tunnel CNAME"
fi

if [ "$app_count" -eq 0 ]; then
    body=$(jq -cn --arg name "$app_name" --arg host "$hostname" --arg policy "$policy_name" --arg email "$access_email" '
        {
          name:$name,
          type:"self_hosted",
          domain:$host,
          destinations:[{type:"public",uri:$host}],
          app_launcher_visible:false,
          session_duration:"24h",
          policies:[{
            name:$policy,
            decision:"allow",
            precedence:1,
            include:[{email:{email:$email}}],
            exclude:[],
            require:[]
          }]
        }
    ')
    cf POST "/accounts/$account_id/access/apps" "$body" > /dev/null || die "failed to create the Access application"
fi

echo "Cloudflare Tunnel, DNS, and Access are configured for $hostname."
echo "Tunnel recovery credential: $tunnel_secret (one Keytap recipient)"
echo "On the Mac Studio, run scripts/cloudflare-install-host.sh and then gremvm restart."
