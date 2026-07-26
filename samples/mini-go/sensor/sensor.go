//go:build windows || tinygo

package sensor

type Device interface {
	Read() int
}

type Sample struct {
	Value int
}

func (sample *Sample) Read() int {
	return sample.Value
}

func Read() int {
	sample := Sample{Value: 42}
	return sample.Read()
}
