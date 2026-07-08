package runner

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
)

const adapterProtocol = "grok.managed-adapter/v1"

type adapterRequest struct {
	Protocol string          `json:"protocol"`
	Type     string          `json:"type"`
	ID       string          `json:"id"`
	Method   string          `json:"method"`
	Params   json.RawMessage `json:"params"`
}

type adapterResponse struct {
	Protocol string          `json:"protocol"`
	Type     string          `json:"type"`
	ID       string          `json:"id"`
	OK       bool            `json:"ok"`
	Result   json.RawMessage `json:"result,omitempty"`
	Error    *adapterError   `json:"error,omitempty"`
}

type adapterError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

type adapterProcess struct {
	command *exec.Cmd
	stdin   io.WriteCloser
	lines   chan []byte
	readErr chan error
	done    chan struct{}
	cancel  context.CancelFunc
	maximum int

	callGate chan struct{}
	killOnce sync.Once
	waitMu   sync.Mutex
	waitErr  error
	nextID   uint64
}

type initializeParams struct {
	IntegrationID      string          `json:"integrationId"`
	IntegrationVersion string          `json:"integrationVersion"`
	ProtocolVersion    string          `json:"protocolVersion"`
	Config             json.RawMessage `json:"config"`
	Grants             []string        `json:"grants"`
	Workspaces         []Workspace     `json:"workspaces"`
}

func launchAdapter(
	processParent context.Context,
	initializeParent context.Context,
	policy Policy,
	bundle *VerifiedBundle,
	stateDirectory string,
	initialization initializeParams,
) (*adapterProcess, error) {
	if _, err := bundle.ReverifyExecutable(); err != nil {
		return nil, err
	}
	mounts, err := prepareSandboxMounts(policy, bundle, stateDirectory, initialization.Workspaces)
	if err != nil {
		return nil, err
	}
	defer mounts.close()
	arguments, err := sandboxArguments(policy, bundle, stateDirectory, initialization.Workspaces)
	if err != nil {
		return nil, err
	}
	// The adapter belongs to the managed instance, not to the transient control
	// request which initialized it.
	processContext, cancel := context.WithCancel(processParent)
	command := exec.CommandContext(processContext, policy.BubblewrapPath, arguments...)
	command.Env = []string{"LANG=C.UTF-8"}
	command.Dir = "/"
	command.ExtraFiles = mounts.files
	command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true, Pdeathsig: syscall.SIGKILL}
	stdin, err := command.StdinPipe()
	if err != nil {
		cancel()
		return nil, errors.New("adapter stdin could not be created")
	}
	stdout, err := command.StdoutPipe()
	if err != nil {
		cancel()
		return nil, errors.New("adapter stdout could not be created")
	}
	stderr, err := command.StderrPipe()
	if err != nil {
		cancel()
		return nil, errors.New("adapter stderr could not be created")
	}
	if err := command.Start(); err != nil {
		cancel()
		return nil, errors.New("adapter process could not be started")
	}
	client := &adapterProcess{
		command: command, stdin: stdin, lines: make(chan []byte, 1), readErr: make(chan error, 1),
		done: make(chan struct{}), cancel: cancel, maximum: bundle.Adapter.Limits.MaxMessageBytes,
		callGate: make(chan struct{}, 1),
	}
	go client.readLoop(stdout)
	go func() { _, _ = io.Copy(io.Discard, stderr) }()
	go func() {
		err := command.Wait()
		client.waitMu.Lock()
		client.waitErr = err
		client.waitMu.Unlock()
		close(client.done)
	}()

	initializeContext, initializeCancel := context.WithTimeout(initializeParent, time.Duration(bundle.Adapter.Limits.InitializeTimeoutMS)*time.Millisecond)
	defer initializeCancel()
	params, err := json.Marshal(initialization)
	if err != nil {
		client.kill()
		return nil, errors.New("adapter initialization could not be encoded")
	}
	if _, err := client.call(initializeContext, "lifecycle.initialize", params); err != nil {
		client.kill()
		return nil, err
	}
	return client, nil
}

type sandboxMounts struct {
	files []*os.File
}

func (mounts *sandboxMounts) close() {
	for _, file := range mounts.files {
		_ = file.Close()
	}
}

func prepareSandboxMounts(
	policy Policy,
	bundle *VerifiedBundle,
	stateDirectory string,
	workspaces []Workspace,
) (*sandboxMounts, error) {
	mounts := &sandboxMounts{}
	fail := func(err error) (*sandboxMounts, error) {
		mounts.close()
		return nil, err
	}
	appendDirectory := func(directory *SecureDir, name string) error {
		fd, err := directory.Dup()
		if err != nil {
			return err
		}
		mounts.files = append(mounts.files, os.NewFile(uintptr(fd), name))
		return nil
	}
	if err := appendDirectory(bundle.Dir, "verified-integration-bundle"); err != nil {
		return fail(err)
	}
	state, err := OpenSecureRoot(stateDirectory, uint32(os.Geteuid()), false)
	if err != nil {
		return fail(errors.New("integration state directory could not be pinned"))
	}
	if err := appendDirectory(state, "integration-state"); err != nil {
		state.Close()
		return fail(err)
	}
	state.Close()

	workspaceRoot, err := OpenSecureRoot(policy.WorkspaceRoot, policy.BundleOwnerUID, false)
	if err != nil {
		return fail(errors.New("workspace root could not be pinned"))
	}
	defer workspaceRoot.Close()
	for _, workspace := range workspaces {
		directory, err := workspaceRoot.OpenDir(workspace.MountID, false)
		if err != nil {
			return fail(errors.New("workspace mount could not be pinned"))
		}
		if err := appendDirectory(directory, "workspace-"+workspace.MountID); err != nil {
			directory.Close()
			return fail(err)
		}
		directory.Close()
	}
	seccomp, err := newAdapterSeccompFile()
	if err != nil {
		return fail(err)
	}
	mounts.files = append(mounts.files, seccomp)
	return mounts, nil
}

func sandboxArguments(
	policy Policy,
	bundle *VerifiedBundle,
	stateDirectory string,
	workspaces []Workspace,
) ([]string, error) {
	if !filepath.IsAbs(stateDirectory) || filepath.Clean(stateDirectory) != stateDirectory {
		return nil, errors.New("integration state path is invalid")
	}
	command := "/opt/grok-integration/" + bundle.Manifest.Entrypoint.Command
	arguments := []string{
		"--die-with-parent", "--new-session", "--unshare-all", "--disable-userns", "--clearenv",
		"--hostname", "grok-integration",
	}
	destinations := []string{"/proc", "/dev", "/tmp", "/nix/store", "/opt/grok-integration", stateDirectory}
	for _, workspace := range workspaces {
		destinations = append(destinations, workspace.Path)
	}
	arguments = append(arguments, sandboxDirectoryArguments(destinations)...)
	arguments = append(arguments,
		"--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp",
		"--ro-bind", "/nix/store", "/nix/store",
		"--ro-bind-fd", "3", "/opt/grok-integration",
		"--bind-fd", "4", stateDirectory,
	)
	for index, workspace := range workspaces {
		arguments = append(arguments, "--ro-bind-fd", strconv.Itoa(5+index), workspace.Path)
	}
	arguments = append(arguments, "--seccomp", strconv.Itoa(5+len(workspaces)))
	arguments = append(arguments,
		"--chdir", stateDirectory,
		"--setenv", "HOME", stateDirectory,
		"--setenv", "TMPDIR", "/tmp",
		"--setenv", "PATH", "/opt/grok-integration/bin",
		"--", command,
	)
	arguments = append(arguments, bundle.Manifest.Entrypoint.Arguments...)
	return arguments, nil
}

func sandboxDirectoryArguments(destinations []string) []string {
	directories := make(map[string]struct{})
	for _, destination := range destinations {
		for parent := filepath.Dir(destination); parent != "/" && parent != "."; parent = filepath.Dir(parent) {
			directories[parent] = struct{}{}
		}
	}
	ordered := make([]string, 0, len(directories))
	for directory := range directories {
		ordered = append(ordered, directory)
	}
	sort.Slice(ordered, func(left, right int) bool {
		leftDepth := strings.Count(ordered[left], string(filepath.Separator))
		rightDepth := strings.Count(ordered[right], string(filepath.Separator))
		if leftDepth != rightDepth {
			return leftDepth < rightDepth
		}
		return ordered[left] < ordered[right]
	})
	arguments := make([]string, 0, len(ordered)*2)
	for _, directory := range ordered {
		arguments = append(arguments, "--dir", directory)
	}
	return arguments
}

func (process *adapterProcess) call(ctx context.Context, method string, params json.RawMessage) (json.RawMessage, error) {
	if err := process.acquireCall(ctx); err != nil {
		process.kill()
		return nil, err
	}
	defer func() { <-process.callGate }()
	if len(params) == 0 || !json.Valid(params) {
		return nil, errors.New("adapter request parameters are invalid")
	}
	if err := ctx.Err(); err != nil {
		process.kill()
		return nil, err
	}
	process.nextID++
	id := fmt.Sprintf("runner-%d", process.nextID)
	request, err := json.Marshal(adapterRequest{Protocol: adapterProtocol, Type: "request", ID: id, Method: method, Params: params})
	if err != nil || len(request) > process.maximum {
		return nil, errors.New("adapter request exceeds its limit")
	}
	request = append(request, '\n')
	writeResult := make(chan error, 1)
	go func() { writeResult <- writeAll(process.stdin, request) }()
	select {
	case err := <-writeResult:
		if err != nil {
			process.kill()
			return nil, errors.New("adapter request could not be written")
		}
		if err := ctx.Err(); err != nil {
			process.kill()
			return nil, err
		}
	case <-process.done:
		process.kill()
		return nil, errors.New("adapter exited while accepting a request")
	case <-ctx.Done():
		process.kill()
		return nil, ctx.Err()
	}
	select {
	case line := <-process.lines:
		if err := ctx.Err(); err != nil {
			process.kill()
			return nil, err
		}
		var response adapterResponse
		if err := strictjson.Decode(line, int64(process.maximum), &response); err != nil {
			process.kill()
			return nil, errors.New("adapter response is invalid")
		}
		if response.Protocol != adapterProtocol || response.Type != "response" || response.ID != id {
			process.kill()
			return nil, errors.New("adapter response correlation failed")
		}
		if response.OK {
			if len(response.Result) == 0 || response.Error != nil || !json.Valid(response.Result) {
				process.kill()
				return nil, errors.New("adapter success response is invalid")
			}
			if err := ctx.Err(); err != nil {
				process.kill()
				return nil, err
			}
			return response.Result, nil
		}
		if response.Error == nil || len(response.Error.Code) == 0 || len(response.Error.Message) > 2048 || len(response.Result) != 0 {
			process.kill()
			return nil, errors.New("adapter error response is invalid")
		}
		if err := ctx.Err(); err != nil {
			process.kill()
			return nil, err
		}
		return nil, fmt.Errorf("adapter rejected request: %s", response.Error.Code)
	case err := <-process.readErr:
		process.kill()
		return nil, fmt.Errorf("adapter protocol stream failed: %w", err)
	case <-process.done:
		process.kill()
		return nil, errors.New("adapter exited before responding")
	case <-ctx.Done():
		process.kill()
		return nil, ctx.Err()
	}
}

func (process *adapterProcess) acquireCall(ctx context.Context) error {
	select {
	case process.callGate <- struct{}{}:
		return nil
	case <-process.done:
		return errors.New("adapter is not running")
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (process *adapterProcess) health(ctx context.Context) error {
	_, err := process.call(ctx, "lifecycle.health", json.RawMessage(`{}`))
	return err
}

func (process *adapterProcess) shutdown(ctx context.Context, reason string) {
	params, _ := json.Marshal(map[string]string{"reason": reason})
	_, _ = process.call(ctx, "lifecycle.shutdown", params)
	if process.command.Process != nil {
		_ = syscall.Kill(-process.command.Process.Pid, syscall.SIGTERM)
	}
	select {
	case <-process.done:
	case <-ctx.Done():
		process.kill()
	}
}

func (process *adapterProcess) kill() {
	process.killOnce.Do(func() {
		if process.cancel != nil {
			process.cancel()
		}
		if process.stdin != nil {
			_ = process.stdin.Close()
		}
		if process.command != nil && process.command.Process != nil {
			_ = syscall.Kill(-process.command.Process.Pid, syscall.SIGKILL)
		}
	})
}

func (process *adapterProcess) readLoop(stdout io.Reader) {
	reader := bufio.NewReaderSize(stdout, min(process.maximum, 64<<10))
	for {
		line, err := readBoundedLine(reader, process.maximum)
		if err != nil {
			process.readErr <- err
			return
		}
		select {
		case process.lines <- line:
		case <-process.done:
			return
		}
	}
}

func readBoundedLine(reader *bufio.Reader, maximum int) ([]byte, error) {
	var buffer bytes.Buffer
	for {
		fragment, more, err := reader.ReadLine()
		if err != nil {
			return nil, err
		}
		if buffer.Len()+len(fragment) > maximum {
			return nil, errors.New("adapter frame exceeds its limit")
		}
		buffer.Write(fragment)
		if !more {
			if buffer.Len() == 0 {
				return nil, errors.New("adapter emitted an empty frame")
			}
			return buffer.Bytes(), nil
		}
	}
}

func writeAll(writer io.Writer, data []byte) error {
	for len(data) > 0 {
		written, err := writer.Write(data)
		if err != nil {
			return err
		}
		if written == 0 {
			return io.ErrUnexpectedEOF
		}
		data = data[written:]
	}
	return nil
}

func (process *adapterProcess) exitError() error {
	process.waitMu.Lock()
	defer process.waitMu.Unlock()
	return process.waitErr
}
