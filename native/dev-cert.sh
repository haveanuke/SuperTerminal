#!/bin/sh
# One-time developer setup: create and trust a self-signed "SuperTerminal
# Dev" codesigning identity in YOUR login keychain. bundle.sh signs with it,
# which keeps the app's identity stable across rebuilds — without it, every
# build is ad-hoc signed, macOS treats each install as a brand-new app, and
# your permission grants (folder access, local network) reset every time.
#
# No secrets are shared: this GENERATES a fresh private key on your machine.
# Expect one macOS password dialog (certificate trust settings).
set -eu

if security find-identity -v -p codesigning 2>/dev/null | grep -q "SuperTerminal Dev"; then
    echo "SuperTerminal Dev identity already exists — nothing to do."
    exit 0
fi

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
openssl req -x509 -newkey rsa:2048 -keyout "$dir/key.pem" -out "$dir/cert.pem" \
    -days 3650 -nodes -subj "/CN=SuperTerminal Dev" \
    -addext "keyUsage=digitalSignature" \
    -addext "extendedKeyUsage=codeSigning" \
    -addext "basicConstraints=CA:false" 2>/dev/null

KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
# PEM imports, not PKCS12: OpenSSL 3 exports a PKCS12 format Apple's
# `security import` rejects ("MAC verification failed").
security import "$dir/key.pem" -k "$KEYCHAIN" -T /usr/bin/codesign
security import "$dir/cert.pem" -k "$KEYCHAIN"
# Opens the macOS password dialog — it is marking this cert trusted for
# code signing, nothing else.
security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$dir/cert.pem"

echo "Created:"
security find-identity -v -p codesigning | grep "SuperTerminal Dev"
echo "Done — native/bundle.sh will now sign with SuperTerminal Dev."
