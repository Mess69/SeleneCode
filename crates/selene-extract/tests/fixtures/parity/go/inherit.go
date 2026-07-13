package main

type Reader interface {
	Read() string
}

type Closer interface {
	Close()
}

type ReadCloser interface {
	Reader
	Closer
}

type Base struct {
	ID string
}

type Service struct {
	Base
	Name string
}
