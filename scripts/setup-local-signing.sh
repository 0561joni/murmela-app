#!/bin/zsh
# One-time setup: create a self-signed code-signing certificate so local
# Murmela builds keep a stable identity. macOS then keeps Microphone and
# Accessibility permissions across rebuilds. No paid Apple account needed.
#
# Undo: open Keychain Access, delete the "Blabber Local Signing" certificate
# and key. Builds then fall back to ad-hoc signing.
set -euo pipefail

NAME="Blabber Local Signing"
BUNDLE_ID="com.murmela.app"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$NAME" "$KEYCHAIN" >/dev/null 2>&1; then
  echo "\"$NAME\" already exists in your login keychain. Nothing to create."
else
  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  cat > "$WORK/cert.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
CNF
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
    -config "$WORK/cert.cnf" -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>/dev/null
  PASS="$(/usr/bin/openssl rand -hex 16)"
  # macOS' keychain needs the legacy PKCS#12 encryption; LibreSSL uses it by default.
  if ! /usr/bin/openssl pkcs12 -export -legacy -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
      -name "$NAME" -out "$WORK/identity.p12" -passout "pass:$PASS" 2>/dev/null; then
    /usr/bin/openssl pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
      -name "$NAME" -out "$WORK/identity.p12" -passout "pass:$PASS"
  fi
  security import "$WORK/identity.p12" -k "$KEYCHAIN" -P "$PASS" -T /usr/bin/codesign
  echo "Created \"$NAME\" in your login keychain."
  echo "macOS may now ask for your password to trust it for code signing."
  if ! security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$WORK/cert.pem"; then
    echo "Trust was not changed. Signing still works; you can trust it later in Keychain Access."
  fi
fi

echo ""
echo "Clearing old Murmela entries from Microphone and Accessibility (one time)."
tccutil reset Microphone "$BUNDLE_ID" || true
tccutil reset Accessibility "$BUNDLE_ID" || true

echo ""
echo "Done. Next steps:"
echo "  1. Rebuild:  npm run tauri -- build"
echo "  2. Open the new Murmela.app and grant Microphone and Accessibility once."
echo "     (If macOS asks whether codesign may use the key, choose \"Always Allow\".)"
echo "  Later rebuilds keep these permissions."
