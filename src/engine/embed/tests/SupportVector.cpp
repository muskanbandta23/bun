#include "Test.h"

TEST(VectorAppends)
{
    Vector<int> vector;
    for (int i = 0; i < 100; ++i)
        vector.append(i);
    EXPECT(vector.size() == 100 && vector[99] == 99);
    EXPECT_CRASH((void)vector[100]);
}
