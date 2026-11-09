package crypto

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/google/uuid"
)

var (
	ErrInvalidKeySize       = errors.New("invalid public key size: must be 32 bytes")
	ErrInvalidSignatureSize = errors.New("invalid signature size: must be 64 bytes")
)

// GenerateKeypair generates a new Ed25519 keypair.
func GenerateKeypair() (ed25519.PublicKey, ed25519.PrivateKey, error) {
	pub, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, nil, fmt.Errorf("generating ed25519 key: %w", err)
	}
	return pub, priv, nil
}

// CanonicalEventMessage formats event ID and raw payload consistently for signing.
// Normalizes JSON payload by compacting whitespace.
func CanonicalEventMessage(eventID uuid.UUID, payload []byte) []byte {
	var buf bytes.Buffer
	compactPayload := payload
	if err := json.Compact(&buf, payload); err == nil {
		compactPayload = buf.Bytes()
	}
	return []byte(fmt.Sprintf("%s:%s", eventID.String(), string(compactPayload)))
}

// SignEvent generates an Ed25519 signature for an event.
func SignEvent(privKey ed25519.PrivateKey, eventID uuid.UUID, payload []byte) []byte {
	msg := CanonicalEventMessage(eventID, payload)
	return ed25519.Sign(privKey, msg)
}

// ParseSignature parses raw signature bytes from hex, base64, or JSON array.
func ParseSignature(raw json.RawMessage) ([]byte, error) {
	if len(raw) == 0 {
		return nil, errors.New("empty signature")
	}

	trimmed := strings.TrimSpace(string(raw))
	if strings.HasPrefix(trimmed, "\"") && strings.HasSuffix(trimmed, "\"") {
		// It's a string
		var s string
		if err := json.Unmarshal(raw, &s); err != nil {
			return nil, fmt.Errorf("unmarshaling signature string: %w", err)
		}

		// Try hex decoding
		if sigBytes, err := hex.DecodeString(s); err == nil && len(sigBytes) == ed25519.SignatureSize {
			return sigBytes, nil
		}

		// Try standard base64 decoding
		if sigBytes, err := base64.StdEncoding.DecodeString(s); err == nil && len(sigBytes) == ed25519.SignatureSize {
			return sigBytes, nil
		}

		return nil, errors.New("signature string is neither valid 64-byte hex nor base64")
	}

	// Try unmarshaling as byte array [1, 2, ...]
	var byteSlice []byte
	if err := json.Unmarshal(raw, &byteSlice); err == nil {
		if len(byteSlice) == ed25519.SignatureSize {
			return byteSlice, nil
		}
	}

	return nil, ErrInvalidSignatureSize
}

// VerifySignature verifies an Ed25519 signature.
// Checks canonical compacted message, raw message, and raw/compacted payload.
func VerifySignature(publicKey []byte, eventID uuid.UUID, payload []byte, signature []byte) bool {
	if len(publicKey) != ed25519.PublicKeySize {
		return false
	}
	if len(signature) != ed25519.SignatureSize {
		return false
	}

	pubKey := ed25519.PublicKey(publicKey)

	// 1. Canonical compacted message: event_id:compacted_payload
	canonicalMsg := CanonicalEventMessage(eventID, payload)
	if ed25519.Verify(pubKey, canonicalMsg, signature) {
		return true
	}

	// 2. Raw uncompacted message: event_id:raw_payload
	rawMsg := []byte(fmt.Sprintf("%s:%s", eventID.String(), string(payload)))
	if ed25519.Verify(pubKey, rawMsg, signature) {
		return true
	}

	// 3. Compacted payload directly
	var buf bytes.Buffer
	if err := json.Compact(&buf, payload); err == nil {
		if ed25519.Verify(pubKey, buf.Bytes(), signature) {
			return true
		}
	}

	// 4. Raw payload directly
	if ed25519.Verify(pubKey, payload, signature) {
		return true
	}

	return false
}
