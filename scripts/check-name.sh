#!/usr/bin/env bash
# Usage: check-name.sh <name>  — prints one line per registry: free / TAKEN (detail)
set -u
n=$(echo "$1" | tr 'A-Z' 'a-z'); UA="arr-name-check/0.1"
hc() { curl -sSL -m 15 -o /dev/null -w "%{http_code}" -A "$UA" "$1"; }
r_crates=$([ "$(hc https://crates.io/api/v1/crates/$n)" = 200 ] && echo TAKEN || echo free)
r_pypi=$([ "$(hc https://pypi.org/pypi/$n/json)" = 200 ] && echo TAKEN || echo free)
r_npm=$(curl -sS -m 15 https://registry.npmjs.org/$n | grep -q '"name"' && echo TAKEN || echo free)
r_dhns=$([ "$(hc https://hub.docker.com/v2/users/$n/)" = 200 ] && echo TAKEN || echo free)
dh=$(curl -sS -m 15 "https://hub.docker.com/v2/search/repositories/?query=$n&page_size=25" | python3 -I -c "
import json,sys; n='$n'; d=json.load(sys.stdin)
ex=[r['repo_name'] for r in d.get('results',[]) if r['repo_name'].split('/')[-1]==n]
print(('TAKEN '+','.join(ex[:3])) if ex else 'free')")
r_brew=$([ "$(hc https://formulae.brew.sh/api/formula/$n.json)" = 200 ] && echo TAKEN || echo free)
r_aur=$(curl -sS -m 15 "https://aur.archlinux.org/rpc/v5/info/$n" | grep -q '"resultcount":0' && echo free || echo TAKEN)
r_repo=$(curl -sS -m 15 -A "$UA" https://repology.org/api/v1/project/$n | grep -q '"repo"' && echo TAKEN || echo free)
doms=""; for t in app dev io org com rs; do c=$(hc https://rdap.org/domain/$n.$t); doms="$doms $t:$([ "$c" = 200 ] && echo X || echo ok)"; done
gh=$(gh api -X GET search/repositories -f q="$n in:name" -f per_page=50 --jq "[.items[] | select((.name|ascii_downcase)==\"$n\")] | \"\(length) exact; top: \" + ([.[] | \"\(.full_name)(★\(.stargazers_count),\(.pushed_at[:7]),\(.description//\"\"|.[:60]))\"] | .[:3] | join(\" | \"))" 2>/dev/null)
printf "%-10s crates:%s pypi:%s npm:%s dockerhub-ns:%s dockerhub-img:%s brew:%s aur:%s repology:%s\n           domains:%s\n           github: %s\n" "$n" "$r_crates" "$r_pypi" "$r_npm" "$r_dhns" "$dh" "$r_brew" "$r_aur" "$r_repo" "$doms" "$gh"
