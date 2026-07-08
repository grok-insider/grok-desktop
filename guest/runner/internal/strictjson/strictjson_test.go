package strictjson

import (
	"strings"
	"testing"
)

func TestDecodeRejectsDuplicateUnknownAndTrailingValues(t *testing.T) {
	type document struct {
		Name string `json:"name"`
	}
	for name, data := range map[string]string{
		"duplicate": `{"name":"one","name":"two"}`,
		"unknown":   `{"name":"one","extra":true}`,
		"trailing":  `{"name":"one"} {}`,
	} {
		t.Run(name, func(t *testing.T) {
			var value document
			if err := Decode([]byte(data), 1024, &value); err == nil {
				t.Fatalf("invalid document was accepted: %s", data)
			}
		})
	}
}

func TestValidateRejectsExcessiveDepthAndInvalidUTF8(t *testing.T) {
	deep := strings.Repeat("[", maxDepth+2) + "0" + strings.Repeat("]", maxDepth+2)
	if err := Validate([]byte(deep), 4096); err == nil {
		t.Fatal("excessively deep JSON was accepted")
	}
	if err := Validate([]byte{'"', 0xff, '"'}, 16); err == nil {
		t.Fatal("invalid UTF-8 was accepted")
	}
}
