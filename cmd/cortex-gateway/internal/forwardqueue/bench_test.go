package forwardqueue

import (
	"context"
	"testing"
)

func BenchmarkAcquireReleaseNoWait(b *testing.B) {
	manager := NewManager(3)
	ctx := context.Background()

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		release, err := manager.Acquire(ctx)
		if err != nil {
			b.Fatal(err)
		}
		release()
	}
}

func BenchmarkAcquireReleaseParallel(b *testing.B) {
	manager := NewManager(3)
	ctx := context.Background()

	b.ReportAllocs()
	b.ResetTimer()

	b.RunParallel(func(pb *testing.PB) {
		for pb.Next() {
			release, err := manager.Acquire(ctx)
			if err != nil {
				b.Fatal(err)
			}
			release()
		}
	})
}

func BenchmarkAcquireReleaseContended(b *testing.B) {
	manager := NewManager(1)
	ctx := context.Background()

	release, err := manager.Acquire(ctx)
	if err != nil {
		b.Fatal(err)
	}
	defer release()

	b.ReportAllocs()
	b.ResetTimer()

	for i := 0; i < b.N; i++ {
		done := make(chan error, 1)
		go func() {
			nextRelease, err := manager.Acquire(ctx)
			if err == nil {
				nextRelease()
			}
			done <- err
		}()

		release()
		if err := <-done; err != nil {
			b.Fatal(err)
		}

		release, err = manager.Acquire(ctx)
		if err != nil {
			b.Fatal(err)
		}
	}
}
