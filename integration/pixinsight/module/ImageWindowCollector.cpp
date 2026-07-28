// ImageWindowCollector.cpp -- see ImageWindowCollector.h.

#include "ImageWindowCollector.h"

#include <cstring>

#include <pcl/Image.h>
#include <pcl/ImageVariant.h>
#include <pcl/View.h>

namespace pcl
{

// ----------------------------------------------------------------------------

void ImageWindowCollector::begin( uint64_t w, uint64_t h, uint64_t ch )
{
   m_width    = w;
   m_height   = h;
   m_channels = ch;

   // Hidden, black, Float32 window; color iff >= 3 channels. Shown by the
   // caller (Task 5), never here.
   m_window = ImageWindow( int( w ), int( h ), int( ch ),
                           32 /*bitsPerSample*/, true /*floatSample*/,
                           ch >= 3 /*color*/, true /*initialProcessing*/,
                           "MergeMosaic" );
}

// ----------------------------------------------------------------------------

void ImageWindowCollector::band( uint64_t y0, uint64_t rows, const float* planar,
                                 uint64_t width, uint64_t ch )
{
   View view = m_window.MainView();
   view.LockForWrite( false /*notify*/ );

   // The output window is Float32 by construction (begin()), so the image
   // resolves to pcl::Image; the mutable ScanLine overload is used (EnsureUnique
   // is a no-op here -- this window is freshly created and not shared).
   ImageVariant image = view.Image();
   Image& img = static_cast<Image&>( *image );

   const uint64_t plane = rows * width;   // samples per channel in this band
   for ( uint64_t c = 0; c < ch; ++c )
   {
      // Inverse of ViewPanelSource: channel c of the band is at
      // planar + c*rows*width; within a channel band rows are contiguous, and
      // canvas rows [y0, y0+rows) of channel c are likewise contiguous, so the
      // whole channel band is one memcpy to ScanLine(y0, c).
      const float* src = planar + c * plane;
      std::memcpy( img.ScanLine( int( y0 ), int( c ) ), src, size_t( plane ) * sizeof( float ) );
   }

   view.UnlockForWrite( false /*notify*/ );
}

// ----------------------------------------------------------------------------

} // namespace pcl
