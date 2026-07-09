package manifestverify

type Manifest struct {
	Schema          string        `json:"$schema,omitempty"`
	ManifestVersion int           `json:"manifestVersion"`
	ID              string        `json:"id"`
	Version         string        `json:"version"`
	Protocol        ProtocolRange `json:"protocol"`
	Entrypoint      Entrypoint    `json:"entrypoint"`
	Publisher       Publisher     `json:"publisher"`
	Signature       Signature     `json:"signature"`
	Capabilities    []string      `json:"capabilities"`
	ConfigSchema    string        `json:"configSchema"`
	Permissions     Permissions   `json:"permissions"`
	UpdateChannel   string        `json:"updateChannel"`
	Lifecycle       Lifecycle     `json:"lifecycle"`
}

type ProtocolRange struct {
	MinInclusive string `json:"minInclusive"`
	MaxExclusive string `json:"maxExclusive"`
}

type Entrypoint struct {
	Command   string   `json:"command"`
	Arguments []string `json:"arguments"`
	Adapter   string   `json:"adapter"`
}

type Publisher struct {
	ID    string `json:"id"`
	Name  string `json:"name"`
	Trust string `json:"trust"`
	URL   string `json:"url,omitempty"`
}

type Signature struct {
	Algorithm string  `json:"algorithm"`
	KeyID     *string `json:"keyId"`
	Value     *string `json:"value"`
}

type Permissions struct {
	Filesystem       FilesystemPermissions `json:"filesystem"`
	Network          NetworkPermissions    `json:"network"`
	Process          ProcessPermissions    `json:"process"`
	Devices          []string              `json:"devices"`
	Secrets          []string              `json:"secrets"`
	HostCapabilities []string              `json:"hostCapabilities"`
}

type FilesystemPermissions struct {
	ReadOnlyRoots  []string `json:"readOnlyRoots"`
	ReadWriteRoots []string `json:"readWriteRoots"`
}

type NetworkPermissions struct {
	Outbound []NetworkEndpoint `json:"outbound"`
	Listen   []ListenEndpoint  `json:"listen"`
}

type NetworkEndpoint struct {
	Host  string `json:"host"`
	Ports []int  `json:"ports"`
	TLS   bool   `json:"tls"`
}

type ListenEndpoint struct {
	Family  string `json:"family"`
	Address string `json:"address"`
}

type ProcessPermissions struct {
	Spawn []string `json:"spawn"`
}

type Lifecycle struct {
	Scope             string      `json:"scope"`
	RestartPolicy     string      `json:"restartPolicy"`
	ShutdownTimeoutMS int         `json:"shutdownTimeoutMs"`
	HealthCheck       HealthCheck `json:"healthCheck"`
}

type HealthCheck struct {
	Method           string `json:"method"`
	IntervalMS       int    `json:"intervalMs"`
	TimeoutMS        int    `json:"timeoutMs"`
	FailureThreshold int    `json:"failureThreshold"`
}
