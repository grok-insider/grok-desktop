package host

import (
	"context"
	"crypto/sha256"
	"errors"
	"sync"
	"time"
)

var (
	errIdempotencyConflict = errors.New("idempotency key reused with a different request")
	errIdempotencyCapacity = errors.New("idempotency cache is full")
)

type idempotencyEntry struct {
	digest   [sha256.Size]byte
	created  time.Time
	ready    chan struct{}
	response ResponseEnvelope
	done     bool
}

type idempotencyCache struct {
	mu         sync.Mutex
	entries    map[string]*idempotencyEntry
	maxEntries int
	ttl        time.Duration
}

func newIdempotencyCache(maxEntries int, ttl time.Duration) *idempotencyCache {
	return &idempotencyCache{
		entries: make(map[string]*idempotencyEntry), maxEntries: maxEntries, ttl: ttl,
	}
}

func (c *idempotencyCache) do(
	ctx context.Context,
	key string,
	digest [sha256.Size]byte,
	now time.Time,
	invoke func() ResponseEnvelope,
) (ResponseEnvelope, error) {
	c.mu.Lock()
	c.pruneLocked(now)
	if existing, ok := c.entries[key]; ok {
		if existing.digest != digest {
			c.mu.Unlock()
			return ResponseEnvelope{}, errIdempotencyConflict
		}
		ready := existing.ready
		c.mu.Unlock()
		select {
		case <-ready:
			c.mu.Lock()
			response := existing.response
			c.mu.Unlock()
			return response, nil
		case <-ctx.Done():
			return ResponseEnvelope{}, ctx.Err()
		}
	}

	if len(c.entries) >= c.maxEntries && !c.evictOldestCompletedLocked() {
		c.mu.Unlock()
		return ResponseEnvelope{}, errIdempotencyCapacity
	}
	entry := &idempotencyEntry{digest: digest, created: now, ready: make(chan struct{})}
	c.entries[key] = entry
	c.mu.Unlock()

	response := invoke()
	c.mu.Lock()
	entry.response = response
	entry.done = true
	close(entry.ready)
	c.mu.Unlock()
	return response, nil
}

func (c *idempotencyCache) pruneLocked(now time.Time) {
	for key, entry := range c.entries {
		if entry.done && now.Sub(entry.created) >= c.ttl {
			delete(c.entries, key)
		}
	}
}

func (c *idempotencyCache) evictOldestCompletedLocked() bool {
	var oldestKey string
	var oldestTime time.Time
	for key, entry := range c.entries {
		if !entry.done {
			continue
		}
		if oldestKey == "" || entry.created.Before(oldestTime) {
			oldestKey = key
			oldestTime = entry.created
		}
	}
	if oldestKey == "" {
		return false
	}
	delete(c.entries, oldestKey)
	return true
}
