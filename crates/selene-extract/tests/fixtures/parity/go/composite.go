package main

import (
	"fmt"
	"net/http"
)

type Handler struct {
	svc *Service
}

func (h *Handler) Serve(w http.ResponseWriter, r *http.Request) {
	user, err := h.svc.GetUser(r.URL.Path)
	if err != nil {
		fmt.Println(err)
		return
	}
	render(w, user)
}

func render(w http.ResponseWriter, u *User) {}
