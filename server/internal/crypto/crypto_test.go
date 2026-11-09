package crypto

import (
	"encoding/hex"
	"encoding/json"
	"testing"

	"github.com/google/uuid"
)

func TestSignAndVerify(t *testing.T) {
	pub, priv, err := GenerateKeypair()
	if err != nil {
		t.Fatalf("failed to generate keypair: %v", err)
	}

	eventID := uuid.New()
	payload := []byte(`{"amount": 1000, "currency": "USD"}`)

	sig := SignEvent(priv, eventID, payload)

	if !VerifySignature(pub, eventID, payload, sig) {
		t.Fatalf("expected signature verification to succeed")
	}

	// Tampered payload
	tamperedPayload := []byte(`{"amount": 2000, "currency": "USD"}`)
	if VerifySignature(pub, eventID, tamperedPayload, sig) {
		t.Fatalf("expected verification to fail for tampered payload")
	}

	// Tampered event ID
	if VerifySignature(pub, uuid.New(), payload, sig) {
		t.Fatalf("expected verification to fail for tampered event ID")
	}

	// Wrong public key
	wrongPub, _, _ := GenerateKeypair()
	if VerifySignature(wrongPub, eventID, payload, sig) {
		t.Fatalf("expected verification to fail with wrong public key")
	}
}

func TestParseSignature(t *testing.T) {
	_, priv, err := GenerateKeypair()
	if err != nil {
		t.Fatalf("failed to generate keypair: %v", err)
	}

	eventID := uuid.New()
	payload := []byte(`{"test": true}`)
	sig := SignEvent(priv, eventID, payload)

	// 1. Hex string in JSON
	hexStr := hex.EncodeToString(sig)
	hexJSON, err := json.Marshal(hexStr)
	if err != nil {
		t.Fatalf("failed to marshal hex: %v", err)
	}

	parsedHex, err := ParseSignature(hexJSON)
	if err != nil {
		t.Fatalf("failed to parse hex signature: %v", err)
	}
	if len(parsedHex) != 64 {
		t.Fatalf("expected 64 bytes, got %d", len(parsedHex))
	}

	// 2. Byte array in JSON
	bytesJSON, err := json.Marshal(sig)
	if err != nil {
		t.Fatalf("failed to marshal bytes: %v", err)
	}

	parsedBytes, err := ParseSignature(bytesJSON)
	if err != nil {
		t.Fatalf("failed to parse byte array signature: %v", err)
	}
	if len(parsedBytes) != 64 {
		t.Fatalf("expected 64 bytes, got %d", len(parsedBytes))
	}
}
