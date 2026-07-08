//go:build linux

package runner

import (
	"encoding/binary"
	"errors"
	"os"
	"runtime"

	"golang.org/x/sys/unix"
)

const (
	seccompDataNumberOffset       = 0
	seccompDataArchitectureOffset = 4
	seccompDataFirstArgument      = 16
)

func newAdapterSeccompFile() (*os.File, error) {
	filters, err := adapterSeccompFilters(runtime.GOARCH)
	if err != nil {
		return nil, err
	}
	fd, err := unix.MemfdCreate("grok-adapter-seccomp", unix.MFD_CLOEXEC|unix.MFD_ALLOW_SEALING)
	if err != nil {
		return nil, errors.New("adapter seccomp descriptor could not be created")
	}
	file := os.NewFile(uintptr(fd), "grok-adapter-seccomp")
	fail := func() (*os.File, error) {
		_ = file.Close()
		return nil, errors.New("adapter seccomp policy could not be prepared")
	}
	for _, filter := range filters {
		var encoded [8]byte
		binary.LittleEndian.PutUint16(encoded[0:2], filter.Code)
		encoded[2] = filter.Jt
		encoded[3] = filter.Jf
		binary.LittleEndian.PutUint32(encoded[4:8], filter.K)
		if _, err := file.Write(encoded[:]); err != nil {
			return fail()
		}
	}
	if _, err := file.Seek(0, 0); err != nil {
		return fail()
	}
	seals := unix.F_SEAL_SEAL | unix.F_SEAL_SHRINK | unix.F_SEAL_GROW | unix.F_SEAL_WRITE
	if _, err := unix.FcntlInt(file.Fd(), unix.F_ADD_SEALS, seals); err != nil {
		return fail()
	}
	return file, nil
}

func adapterSeccompFilters(goarch string) ([]unix.SockFilter, error) {
	auditArchitecture := uint32(0)
	switch goarch {
	case "amd64":
		auditArchitecture = unix.AUDIT_ARCH_X86_64
	case "arm64":
		auditArchitecture = unix.AUDIT_ARCH_AARCH64
	default:
		return nil, errors.New("adapter seccomp architecture is unsupported")
	}

	loadWord := func(offset uint32) unix.SockFilter {
		return unix.SockFilter{Code: unix.BPF_LD | unix.BPF_W | unix.BPF_ABS, K: offset}
	}
	jumpEqual := func(value uint32, jumpTrue, jumpFalse uint8) unix.SockFilter {
		return unix.SockFilter{Code: unix.BPF_JMP | unix.BPF_JEQ | unix.BPF_K, Jt: jumpTrue, Jf: jumpFalse, K: value}
	}
	returnValue := func(value uint32) unix.SockFilter {
		return unix.SockFilter{Code: unix.BPF_RET | unix.BPF_K, K: value}
	}

	// The runner and mount broker own all VSOCK access. Adapters may create only
	// Unix-domain sockets; any host-facing capability is supplied as an already
	// connected descriptor by a future capability-specific broker. io_uring setup
	// is denied as well so an adapter cannot route socket creation around seccomp.
	return []unix.SockFilter{
		loadWord(seccompDataArchitectureOffset),
		jumpEqual(auditArchitecture, 1, 0),
		returnValue(unix.SECCOMP_RET_KILL_PROCESS),
		loadWord(seccompDataNumberOffset),
		jumpEqual(uint32(unix.SYS_IO_URING_SETUP), 0, 1),
		returnValue(unix.SECCOMP_RET_ERRNO | uint32(unix.EPERM)),
		jumpEqual(uint32(unix.SYS_SOCKET), 1, 0),
		jumpEqual(uint32(unix.SYS_SOCKETPAIR), 0, 3),
		loadWord(seccompDataFirstArgument),
		jumpEqual(unix.AF_UNIX, 1, 0),
		returnValue(unix.SECCOMP_RET_ERRNO | uint32(unix.EPERM)),
		returnValue(unix.SECCOMP_RET_ALLOW),
	}, nil
}
