#include <stdio.h>
#include <stdint.h>
#define HALVES(hi16, lo16) ((((uint32_t)(hi16) & 0xFFFFu) << 16) | ((uint32_t)(lo16) & 0xFFFFu))
int main(void){ printf("HALVES(-0x20,0) = 0x%08X\n", HALVES(-0x20, 0));
                printf("HALVES(0xE0,0)  = 0x%08X\n", HALVES(0xE0, 0)); return 0; }
