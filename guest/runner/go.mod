module github.com/grok-insider/grok-desktop/guest/runner

go 1.25.0

require (
	github.com/grok-insider/grok-desktop/native/windows-vm-service v0.0.0
	github.com/mdlayher/vsock v1.3.0
	github.com/santhosh-tekuri/jsonschema/v5 v5.3.1
	golang.org/x/sys v0.45.0
)

require (
	github.com/mdlayher/socket v0.6.0 // indirect
	golang.org/x/net v0.55.0 // indirect
	golang.org/x/sync v0.20.0 // indirect
	google.golang.org/protobuf v1.36.11 // indirect
)

replace github.com/grok-insider/grok-desktop/native/windows-vm-service => ../../native/windows-vm-service
