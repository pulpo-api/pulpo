#!/usr/bin/env bash
# Runs the code `pulpo code` generates, in every language whose tools are installed,
# against a local echo server, and checks that what arrives is what Pulpo would send.
#   PULPO=target/debug/pulpo bash tests/codegen/run.sh
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
P=$(realpath "${PULPO:-/work/target/debug/pulpo}")
SUDO=$([ "$(id -u)" = 0 ] || echo sudo)
WS=$(mktemp -d)
cd "$WS"

python3 "$HERE/echo.py" 8099 >/tmp/echo.log 2>&1 &
ECHO=$!
trap 'kill $ECHO' EXIT
sleep 1

# Tools for axios and requests, outside the repo.
mkdir -p /tmp/js && (cd /tmp/js && [ -d node_modules/axios ] || npm init -y >/dev/null && npm install axios@1 --silent >/dev/null 2>&1)
python3 -c "import requests" 2>/dev/null || ($SUDO apt-get update -qq && $SUDO apt-get install -y -qq python3-requests) >/dev/null 2>&1

$P init . --name Smoke >/dev/null
ln -s /tmp/js/node_modules node_modules
mkdir -p environments files
cat > environments/local.yaml <<'YAML'
version: 1
name: "Local"
variables:
  - name: "base"
    value: "http://127.0.0.1:8099"
  - name: "token"
    secret: true
YAML
printf 'hola pulpo' > files/hola.txt
printf '{"bin":true}' > files/data.json

req() { # file, then YAML lines after the url
  printf 'version: 1\nname: "%s"\nprotocol: "http"\n%s\n' "$1" "$2" > "$1.pulpo.yaml"
}
req 01-json 'method: "POST"
url: "{{base}}/orders/:id"
pathParams:
  - name: "id"
    value: "a b"
headers:
  - name: "Content-Type"
    value: "application/json"
auth:
  type: "bearer"
  token: "{{token}}"
body: "json"'
printf '{"note": "it'"'"'s \\"ok\\" ñ"}' > 01-json.body.json
req 02-basic 'method: "GET"
url: "{{base}}/me?x=1"
auth:
  type: "basic"
  username: "ana"
  password: "p@ss:wd"'
req 03-form 'method: "POST"
url: "{{base}}/login"
body: "urlencoded"
form:
  - name: "user"
    value: "ana maría"
  - name: "q"
    value: "a&b=c"'
req 04-multipart 'method: "POST"
url: "{{base}}/upload"
body: "multipart"
form:
  - name: "title"
    value: "Hola"
  - name: "doc"
    file: "files/hola.txt"'
req 05-file 'method: "PUT"
url: "{{base}}/raw"
body: "file"
bodyFile: "files/data.json"'
req 06-apikey 'method: "DELETE"
url: "{{base}}/items"
auth:
  type: "apiKey"
  key: "api key"
  value: "k&1"
  in: "query"'

export PULPO_SECRET_TOKEN=t0ken
fails=0
check() { # name, language, output, expected substrings...
  local name=$1 lang=$2 out=$3; shift 3
  for want in "$@"; do
    if ! grep -qF -- "$want" <<<"$out"; then
      echo "FAIL $name/$lang: missing [$want]"; echo "$out" | head -20; fails=$((fails+1)); return
    fi
  done
  echo "ok   $name/$lang"
}

# Every language whose tools are installed (the GitHub Ubuntu runner has them all).
langs="curl fetch axios python"
command -v go >/dev/null && langs="$langs go"
command -v java >/dev/null && langs="$langs java"
command -v dotnet >/dev/null && { dotnet new console -o /tmp/cs --force >/dev/null 2>&1 && langs="$langs csharp"; }
command -v pwsh >/dev/null && langs="$langs powershell"
echo "languages: $langs"

for lang in $langs; do
  for name in 01-json 02-basic 03-form 04-multipart 05-file 06-apikey; do
    if [ "$lang/$name" = java/04-multipart ]; then
      echo "skip $name/$lang (Java's HttpClient has no multipart builder; the code says so)"; continue
    fi
    code=$($P code "$name.pulpo.yaml" -e Local -l $lang --resolve --secrets)
    case $lang in
      curl) out=$(bash -c "$code" 2>&1) ;;
      # axios prints response.data as an object; print it as JSON so the checks match.
      fetch|axios) { echo 'console.log = (...a) => process.stdout.write(a.map((x) => (typeof x === "string" ? x : JSON.stringify(x, null, 1))).join(" ") + "\n");'; printf '%s\n' "$code"; } > "$WS/snippet.mjs"
        out=$(cd "$WS" && node snippet.mjs 2>&1) ;;
      python) out=$(python3 -c "$code" 2>&1) ;;
      go) mkdir -p /tmp/go && printf '%s\n' "$code" > /tmp/go/main.go; out=$(go run /tmp/go/main.go 2>&1) ;;
      java) mkdir -p /tmp/java && printf '%s\n' "$code" > /tmp/java/Main.java; out=$(java /tmp/java/Main.java 2>&1) ;;
      csharp) printf '%s\n' "$code" > /tmp/cs/Program.cs; out=$(dotnet run --project /tmp/cs 2>&1) ;;
      powershell) printf '%s\n' "$code" > /tmp/snippet.ps1; out=$(pwsh -NoProfile -File /tmp/snippet.ps1 2>&1) ;;
    esac
    case $name in
      01-json) check $name $lang "$out" '"method": "POST"' '"path": "/orders/a%20b"' '"authorization": "Bearer t0ken"' '\"note' 'ñ' "it's" ;;
      02-basic) check $name $lang "$out" '"method": "GET"' '"path": "/me?x=1"' '"authorization": "Basic YW5hOnBAc3M6d2Q="' ;;
      03-form) check $name $lang "$out" '"content-type": "application/x-www-form-urlencoded' 'user=ana' 'q=a%26b%3Dc' ;;
      # .NET (C#, PowerShell) writes name=title without quotes: valid for simple names.
      04-multipart) check $name $lang "$out" 'multipart/form-data; boundary=' 'form-data; name=' 'title' 'Hola' 'filename=' 'hola.txt' 'hola pulpo' ;;
      05-file) check $name $lang "$out" '"method": "PUT"' '"content-type": "application/json"' '{\"bin\":true}' ;;
      06-apikey) check $name $lang "$out" '"method": "DELETE"' '"path": "/items?api%20key=k%261"' ;;
    esac
  done
done
echo "failures: $fails"
[ "$fails" -eq 0 ]
