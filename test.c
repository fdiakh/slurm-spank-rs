#include <stdio.h>
#include <slurm/spank.h>
#include <string.h>
#include <stdarg.h>

struct spank_handle
{
};

extern char *plugin_name;
extern char *plugin_type;
extern int slurm_spank_init(spank_t sp, int ac, char **av);

struct spank_option saved_opt;

spank_context_t spank_context(void)
{
        return S_CTX_LOCAL;
}
spank_err_t spank_option_register(spank_t sp,
                                  struct spank_option *opt)
{
        saved_opt.cb = opt->cb;
        saved_opt.val = opt->val;
        saved_opt.has_arg = opt->has_arg;
        if (saved_opt.has_arg && saved_opt.arginfo)
        {
                saved_opt.arginfo = strdup(opt->arginfo);
        }
}
const char *spank_strerror(spank_err_t err)
{
        switch (err)
        {
        case ESPANK_SUCCESS:
                return "Success";
        case ESPANK_ERROR:
                return "Generic error";
        case ESPANK_BAD_ARG:
                return "Bad argument";
        case ESPANK_NOT_TASK:
                return "Not in task context";
        case ESPANK_ENV_EXISTS:
                return "Environment variable exists";
        case ESPANK_ENV_NOEXIST:
                return "No such environment variable";
        case ESPANK_NOSPACE:
                return "Buffer too small";
        case ESPANK_NOT_REMOTE:
                return "Valid only in remote context";
        case ESPANK_NOEXIST:
                return "Id/PID does not exist on this node";
        case ESPANK_NOT_EXECD:
                return "Lookup by PID requested, but no tasks running";
        case ESPANK_NOT_AVAIL:
                return "Item not available from this callback";
        case ESPANK_NOT_LOCAL:
                return "Valid only in local or allocator context";
        }

        return "Unknown";
}

int main(int argc, char **argv)
{
        int err;
        printf("plugin_name: %s\nplugin_type: %s\n", plugin_name, plugin_type);
        err = slurm_spank_init(NULL, argc, argv);
        printf("slurm_spank_init returned %d \n", err);

        saved_opt.cb(saved_opt.val, "toto", 0);

        err = slurm_spank_exit(NULL, argc, argv);
        printf("slurm_spank_exit returned %d \n", err);
}

void slurm_error(const char *fmt, ...)
{
        va_list args;
        va_start(args, fmt);

        fprintf(stderr, "toto\n");
        fprintf(stderr, "fmt: %s\n", fmt);
        vfprintf(stderr, fmt, args);

        va_end(args);
}
