package manifestverify

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

var semanticVersionPattern = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$`)

type semanticVersion struct {
	major      uint64
	minor      uint64
	patch      uint64
	prerelease []string
}

func parseSemanticVersion(value string) (semanticVersion, error) {
	matches := semanticVersionPattern.FindStringSubmatch(value)
	if matches == nil {
		return semanticVersion{}, fmt.Errorf("value is not semantic version 2.0")
	}
	parts := make([]uint64, 3)
	for index := range parts {
		parsed, err := strconv.ParseUint(matches[index+1], 10, 64)
		if err != nil {
			return semanticVersion{}, fmt.Errorf("parse semantic version component: %w", err)
		}
		parts[index] = parsed
	}
	var prerelease []string
	if matches[4] != "" {
		prerelease = strings.Split(matches[4], ".")
		for _, identifier := range prerelease {
			if identifier == "" {
				return semanticVersion{}, fmt.Errorf("prerelease contains an empty identifier")
			}
			if len(identifier) > 1 && identifier[0] == '0' && numeric(identifier) {
				return semanticVersion{}, fmt.Errorf("numeric prerelease identifiers cannot have leading zeroes")
			}
		}
	}
	if matches[5] != "" {
		for _, identifier := range strings.Split(matches[5], ".") {
			if identifier == "" {
				return semanticVersion{}, fmt.Errorf("build metadata contains an empty identifier")
			}
		}
	}
	return semanticVersion{major: parts[0], minor: parts[1], patch: parts[2], prerelease: prerelease}, nil
}

func compareSemanticVersion(left, right semanticVersion) int {
	leftParts := [...]uint64{left.major, left.minor, left.patch}
	rightParts := [...]uint64{right.major, right.minor, right.patch}
	for index := range leftParts {
		if leftParts[index] < rightParts[index] {
			return -1
		}
		if leftParts[index] > rightParts[index] {
			return 1
		}
	}
	if len(left.prerelease) == 0 && len(right.prerelease) == 0 {
		return 0
	}
	if len(left.prerelease) == 0 {
		return 1
	}
	if len(right.prerelease) == 0 {
		return -1
	}
	limit := min(len(left.prerelease), len(right.prerelease))
	for index := 0; index < limit; index++ {
		leftIdentifier := left.prerelease[index]
		rightIdentifier := right.prerelease[index]
		leftNumeric := numeric(leftIdentifier)
		rightNumeric := numeric(rightIdentifier)
		switch {
		case leftNumeric && rightNumeric:
			if len(leftIdentifier) < len(rightIdentifier) {
				return -1
			}
			if len(leftIdentifier) > len(rightIdentifier) {
				return 1
			}
			if leftIdentifier < rightIdentifier {
				return -1
			}
			if leftIdentifier > rightIdentifier {
				return 1
			}
		case leftNumeric:
			return -1
		case rightNumeric:
			return 1
		default:
			if leftIdentifier < rightIdentifier {
				return -1
			}
			if leftIdentifier > rightIdentifier {
				return 1
			}
		}
	}
	if len(left.prerelease) < len(right.prerelease) {
		return -1
	}
	if len(left.prerelease) > len(right.prerelease) {
		return 1
	}
	return 0
}

func numeric(value string) bool {
	if value == "" {
		return false
	}
	for _, character := range value {
		if character < '0' || character > '9' {
			return false
		}
	}
	return true
}
