//go:build linux

package runner

import (
	"errors"
	"os"
	"os/exec"
	"runtime"
	"testing"
	"unsafe"

	"golang.org/x/sys/unix"
)

func TestAdapterSeccompAllowsOnlyUnixSocketCreation(t *testing.T) {
	if os.Getenv("GROK_SECCOMP_HELPER") == "1" {
		runtime.LockOSThread()
		filters, err := adapterSeccompFilters(runtime.GOARCH)
		if err != nil {
			os.Exit(10)
		}
		program := unix.SockFprog{Len: uint16(len(filters)), Filter: &filters[0]}
		if err := unix.Prctl(unix.PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0); err != nil {
			os.Exit(11)
		}
		if err := unix.Prctl(unix.PR_SET_SECCOMP, unix.SECCOMP_MODE_FILTER, uintptr(unsafe.Pointer(&program)), 0, 0); err != nil {
			os.Exit(12)
		}
		unixSocket, err := unix.Socket(unix.AF_UNIX, unix.SOCK_STREAM|unix.SOCK_CLOEXEC, 0)
		if err != nil {
			os.Exit(13)
		}
		_ = unix.Close(unixSocket)
		pair, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_STREAM|unix.SOCK_CLOEXEC, 0)
		if err != nil {
			os.Exit(14)
		}
		_ = unix.Close(pair[0])
		_ = unix.Close(pair[1])
		for _, family := range []int{unix.AF_VSOCK, unix.AF_INET, unix.AF_INET6, unix.AF_NETLINK} {
			if descriptor, err := unix.Socket(family, unix.SOCK_STREAM|unix.SOCK_CLOEXEC, 0); !errors.Is(err, unix.EPERM) {
				if err == nil {
					_ = unix.Close(descriptor)
				}
				os.Exit(15)
			}
		}
		if _, _, errno := unix.Syscall(uintptr(unix.SYS_IO_URING_SETUP), 0, 0, 0); !errors.Is(errno, unix.EPERM) {
			os.Exit(16)
		}
		os.Exit(0)
	}

	command := exec.Command(os.Args[0], "-test.run=^TestAdapterSeccompAllowsOnlyUnixSocketCreation$")
	command.Env = append(os.Environ(), "GROK_SECCOMP_HELPER=1")
	if output, err := command.CombinedOutput(); err != nil {
		t.Fatalf("seccomp helper failed: %v (%s)", err, output)
	}
}

func TestAdapterSeccompDescriptorIsSealed(t *testing.T) {
	file, err := newAdapterSeccompFile()
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	seals, err := unix.FcntlInt(file.Fd(), unix.F_GET_SEALS, 0)
	if err != nil {
		t.Fatal(err)
	}
	want := unix.F_SEAL_SEAL | unix.F_SEAL_SHRINK | unix.F_SEAL_GROW | unix.F_SEAL_WRITE
	if seals&want != want {
		t.Fatalf("seccomp descriptor seals = %#x, want %#x", seals, want)
	}
}

func TestAdapterSeccompRejectsUnsupportedArchitecture(t *testing.T) {
	if _, err := adapterSeccompFilters("unsupported"); err == nil {
		t.Fatal("unsupported seccomp architecture was accepted")
	}
}
