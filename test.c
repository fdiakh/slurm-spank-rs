#include <stdio.h>
#include <slurm/spank.h>

struct spank_handle {

};

extern char * plugin_name;
extern char * plugin_type;
extern int slurm_spank_init(spank_t sp, int ac, char **av);

spank_context_t spank_context (void) {
  return 35;
}

const char * spank_strerror (spank_err_t err)
{
        switch (err) {
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

int main(int argc, char** argv) {
  int err;
  printf("plugin_name: %s\nplugin_type: %s\n", plugin_name, plugin_type);
  err = slurm_spank_init (NULL, argc, argv);
  if (err != 0 ) {
    printf("slurm_spank_init returned %d", err);
  }

}
