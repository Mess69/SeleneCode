#include <string>
#include "service.h"

namespace app {

class Service {
public:
    explicit Service(Client *client);
    std::string fetch(const std::string &id);

private:
    Client *client_;
};

Service::Service(Client *client) : client_(client) {}

std::string Service::fetch(const std::string &id) {
    return client_->get(id);
}

Service *makeService(Client *client) {
    return new Service(client);
}

}  // namespace app
