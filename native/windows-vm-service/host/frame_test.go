package host

import (
	"bufio"
	"bytes"
	"errors"
	"io"
	"testing"
	"time"
)

func TestReadFrameBoundsAndResynchronizes(t *testing.T) {
	reader := bufio.NewReaderSize(bytes.NewBufferString("12345\nok\n"), 2)
	if _, err := readFrame(reader, 4); !errors.Is(err, errFrameTooLarge) {
		t.Fatalf("oversized frame error = %v", err)
	}
	frame, err := readFrame(reader, 4)
	if err != nil {
		t.Fatalf("read frame after oversized input: %v", err)
	}
	if string(frame) != "ok" {
		t.Fatalf("frame = %q, want ok", frame)
	}
}

func TestReadFrameRequiresNewline(t *testing.T) {
	_, err := readFrame(bufio.NewReader(bytes.NewBufferString("partial")), 32)
	if !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("error = %v, want io.ErrUnexpectedEOF", err)
	}
}

func FuzzReadFrame(f *testing.F) {
	f.Add([]byte("{}"), uint16(32))
	f.Add(bytes.Repeat([]byte("x"), 100), uint16(16))
	f.Fuzz(func(t *testing.T, input []byte, rawLimit uint16) {
		limit := int(rawLimit%4096) + 1
		framed := append(append([]byte(nil), input...), '\n')
		frame, err := readFrame(bufio.NewReaderSize(bytes.NewReader(framed), 17), limit)
		if err == nil && len(frame) > limit {
			t.Fatalf("read %d bytes with limit %d", len(frame), limit)
		}
		firstLine := input
		if newline := bytes.IndexByte(firstLine, '\n'); newline >= 0 {
			firstLine = firstLine[:newline]
		}
		firstLine = bytes.TrimSuffix(firstLine, []byte{'\r'})
		if len(firstLine) > limit && !errors.Is(err, errFrameTooLarge) {
			t.Fatalf("oversized input returned error %v", err)
		}
	})
}

func FuzzDecodeEnvelope(f *testing.F) {
	f.Add([]byte(`{"version":"1.0.0","id":"request-1","operation":"get_capabilities","deadline":"2030-01-01T00:00:00Z","payload":{}}`))
	f.Add([]byte(`{`))
	f.Fuzz(func(t *testing.T, input []byte) {
		_, _ = decodeEnvelope(input, time.Unix(0, 0).UTC(), time.Minute)
	})
}
