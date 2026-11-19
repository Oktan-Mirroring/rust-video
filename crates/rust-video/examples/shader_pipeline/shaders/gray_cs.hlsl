// NV12 grayscale: luma passthrough, chroma pinned to neutral grey.
//
// The luma dispatch covers the target; chroma threads guard the half
// extent. `src_origin` carries the decode crop offset in luma samples.
Texture2D<float> src_y : register(t0);
RWTexture2D<float> dst_y : register(u0);
RWTexture2D<float2> dst_uv : register(u1);

cbuffer Origin : register(b0)
{
    uint2 src_origin;
};

[numthreads(16, 16, 1)]
void main(uint3 id : SV_DispatchThreadID)
{
    uint w;
    uint h;
    dst_y.GetDimensions(w, h);
    if (id.x >= w || id.y >= h)
    {
        return;
    }
    dst_y[id.xy] = src_y[id.xy + src_origin];
    if (id.x < w / 2 && id.y < h / 2)
    {
        dst_uv[id.xy] = float2(0.5, 0.5);
    }
}
