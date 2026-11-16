/* theo-pod libFuzzer entry for opencore-aacdec (t-169): see driver.cpp. */
#include <stddef.h>
#include <stdint.h>

struct theo_fuzz_summary {
	int init, frames, decoded;
};

extern "C" struct theo_fuzz_summary theo_fuzz_one(const uint8_t *data, size_t size);

extern "C" int LLVMFuzzerTestOneInput(const uint8_t *data, size_t size)
{
	theo_fuzz_one(data, size);
	return 0;
}
