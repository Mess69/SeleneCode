#include <memory>
#include "widget.h"

namespace app {

class Renderer {
public:
    void render(Widget* w);
private:
    std::unique_ptr<Widget> current;
};

void Renderer::render(Widget* w) {
    w->draw();
    current.reset(w);
}

}  // namespace app
