// ImageWindowCollector.cpp -- see ImageWindowCollector.h.

#include "ImageWindowCollector.h"

#include <cstring>

#include <pcl/Exception.h>
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
                           "MegaMergeMosaic" );

   // Belt-and-braces at the handoff: every band() memcpy below is bounded by
   // the geometry REQUESTED here, but the image it writes into is the one the
   // core actually allocated -- refuse to continue if the two differ in any
   // way (they never should; a mismatch means the write bounds are void).
   // The const ImageVariant keeps this read-only (no EnsureUnique).
   const ImageVariant image = m_window.MainView().Image();
   if ( uint64_t( image.Width() ) != w
     || uint64_t( image.Height() ) != h
     || uint64_t( image.NumberOfChannels() ) != ch
     || !image.IsFloatSample()
     || image.BitsPerSample() != 32 )
   {
      m_window.Close();
      m_window = ImageWindow::Null();
      throw Error( String().Format( "MegaMergeMosaic: internal error: the core created a "
                                    "%dx%dx%d (%d-bit, %s) window for a %llux%llux%llu Float32 request.",
                                    image.Width(), image.Height(), image.NumberOfChannels(),
                                    image.BitsPerSample(), image.IsFloatSample() ? "float" : "integer",
                                    (unsigned long long)w, (unsigned long long)h,
                                    (unsigned long long)ch ) );
   }
}

// ----------------------------------------------------------------------------

void ImageWindowCollector::band( uint64_t y0, uint64_t rows, const float* planar,
                                 uint64_t width, uint64_t ch )
{
   View view = m_window.MainView();
   ViewWriteLockGuard lock( view );

   // The output window is Float32 by construction (begin(), which also
   // verified the created image against the requested geometry), so the image
   // resolves to pcl::Image; the mutable ScanLine overload is used
   // (EnsureUnique is a no-op here -- this window is freshly created and not
   // shared).
   ImageVariant image = view.Image();
   Image& img = static_cast<Image&>( *image );

   // Last-write-site guard: these values parameterize every memcpy below.
   // The host has already validated them against Begin, and begin() verified
   // the window against the same geometry, so a mismatch here can only mean
   // internal state corruption -- refuse rather than overrun the core-owned
   // channel planes.
   if ( width != uint64_t( img.Width() )
     || ch != uint64_t( img.NumberOfChannels() )
     || rows == 0
     || y0 > uint64_t( img.Height() )
     || rows > uint64_t( img.Height() ) - y0 )
      throw Error( String().Format( "MegaMergeMosaic: internal error: band [%llu, %llu+%llu) x %llu "
                                    "ch %llu does not fit the %dx%dx%d output image.",
                                    (unsigned long long)y0, (unsigned long long)y0,
                                    (unsigned long long)rows, (unsigned long long)width,
                                    (unsigned long long)ch,
                                    img.Width(), img.Height(), img.NumberOfChannels() ) );

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
}

// ----------------------------------------------------------------------------

} // namespace pcl
