package host

import (
	"bufio"
	"errors"
	"io"
)

var errFrameTooLarge = errors.New("frame exceeds maximum message size")

// readFrame reads one newline-delimited frame without allowing bufio to grow an
// attacker-controlled allocation. Oversized input is drained through its newline
// so the connection remains synchronized.
func readFrame(reader *bufio.Reader, maxBytes int) ([]byte, error) {
	frame := make([]byte, 0, min(maxBytes, 4096))
	size := 0

	for {
		fragment, err := reader.ReadSlice('\n')
		complete := err == nil
		if complete {
			fragment = fragment[:len(fragment)-1]
		}
		size += len(fragment)
		if size <= maxBytes+1 {
			frame = append(frame, fragment...)
		}

		switch {
		case complete:
			if len(frame) > 0 && frame[len(frame)-1] == '\r' {
				frame = frame[:len(frame)-1]
				size--
			}
			if size > maxBytes {
				return nil, errFrameTooLarge
			}
			return frame, nil
		case errors.Is(err, bufio.ErrBufferFull):
			continue
		case errors.Is(err, io.EOF):
			if size == 0 {
				return nil, io.EOF
			}
			if size > maxBytes {
				return nil, errFrameTooLarge
			}
			return nil, io.ErrUnexpectedEOF
		default:
			return nil, err
		}
	}
}
