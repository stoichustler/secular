#include <err.h>
#include <services/smc/acl.h>

static int allow_all(uint32_t smc_nr) {
    (void)smc_nr;
    return NO_ERROR;
}

static int allow_request(uint32_t smc_nr, const struct uuid *uuid,
                         const struct smc_msg *request) {
    (void)smc_nr;
    (void)uuid;
    (void)request;
    return NO_ERROR;
}

void smc_load_access_policy(const struct uuid *uuid, struct smc_access_policy *policy) {
    (void)uuid;
    policy->check_access = allow_all;
    policy->check_request = allow_request;
}
