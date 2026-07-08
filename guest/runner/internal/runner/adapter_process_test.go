package runner

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"sync"
	"testing"
	"time"
)

type blockingWriteCloser struct {
	entered   chan struct{}
	closed    chan struct{}
	enterOnce sync.Once
	closeOnce sync.Once
}

func newBlockingWriteCloser() *blockingWriteCloser {
	return &blockingWriteCloser{entered: make(chan struct{}), closed: make(chan struct{})}
}

func (writer *blockingWriteCloser) Write([]byte) (int, error) {
	writer.enterOnce.Do(func() { close(writer.entered) })
	<-writer.closed
	return 0, io.ErrClosedPipe
}

func (writer *blockingWriteCloser) Close() error {
	writer.closeOnce.Do(func() { close(writer.closed) })
	return nil
}

type recordingWriteCloser struct {
	mu     sync.Mutex
	writes int
	closed bool
}

func (writer *recordingWriteCloser) Write(data []byte) (int, error) {
	writer.mu.Lock()
	defer writer.mu.Unlock()
	if writer.closed {
		return 0, io.ErrClosedPipe
	}
	writer.writes++
	return len(data), nil
}

func (writer *recordingWriteCloser) Close() error {
	writer.mu.Lock()
	writer.closed = true
	writer.mu.Unlock()
	return nil
}

func newAdapterProcessForTest(writer io.WriteCloser) (*adapterProcess, <-chan struct{}) {
	killed := make(chan struct{})
	process := &adapterProcess{
		stdin: writer, lines: make(chan []byte, 1), readErr: make(chan error, 1),
		done: make(chan struct{}), maximum: 4096, callGate: make(chan struct{}, 1),
		cancel: func() { close(killed) },
	}
	return process, killed
}

func TestAdapterCallDeadlineInterruptsBlockedWriteAndPoisonsProcess(t *testing.T) {
	writer := newBlockingWriteCloser()
	process, killed := newAdapterProcessForTest(writer)
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	result := make(chan error, 1)
	go func() {
		_, err := process.call(ctx, "computer-use.observe", json.RawMessage(`{}`))
		result <- err
	}()
	select {
	case <-writer.entered:
	case <-time.After(time.Second):
		t.Fatal("adapter write did not begin")
	}
	if err := <-result; !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("call error = %v, want deadline exceeded", err)
	}
	select {
	case <-killed:
	default:
		t.Fatal("timed-out adapter process was not poisoned")
	}
}

func TestAdapterCallDoesNotWriteAfterDeadlineWhileWaitingForInflightCall(t *testing.T) {
	writer := &recordingWriteCloser{}
	process, killed := newAdapterProcessForTest(writer)
	process.callGate <- struct{}{}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if _, err := process.call(ctx, "computer-use.observe", json.RawMessage(`{}`)); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("call error = %v, want deadline exceeded", err)
	}
	writer.mu.Lock()
	writes := writer.writes
	writer.mu.Unlock()
	if writes != 0 {
		t.Fatalf("expired call wrote %d adapter frames", writes)
	}
	select {
	case <-killed:
	default:
		t.Fatal("deadline while queued did not poison the adapter")
	}
}

func TestAdapterCallPoisonsStreamAfterCorrelationFailure(t *testing.T) {
	writer := &recordingWriteCloser{}
	process, killed := newAdapterProcessForTest(writer)
	process.lines <- []byte(`{"protocol":"grok.managed-adapter/v1","type":"response","id":"stale","ok":true,"result":{}}`)
	if _, err := process.call(context.Background(), "computer-use.observe", json.RawMessage(`{}`)); err == nil {
		t.Fatal("stale adapter response was accepted")
	}
	select {
	case <-killed:
	default:
		t.Fatal("ambiguous adapter stream was not poisoned")
	}
}
