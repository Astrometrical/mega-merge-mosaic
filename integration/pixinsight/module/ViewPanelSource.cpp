// ViewPanelSource.cpp -- see ViewPanelSource.h.

#include "ViewPanelSource.h"

#include <cstring>

#include <pcl/Image.h>
#include <pcl/ImageVariant.h>

namespace pcl
{

// ----------------------------------------------------------------------------

ViewPanelSource::ViewPanelSource( const Array<View>& views )
   : m_views( views )
{
}

// ----------------------------------------------------------------------------
// Element-wise convert one channel plane (rows*width samples starting at row
// y0 of channel c) to normalized Float32 via the pixel traits. FromSample()
// maps an integer sample to [0,1] and a floating sample through unchanged --
// exactly PixInsight's own normalization (PixelTraits.h). The const ScanLine
// overload is used so a shared image is never copy-on-write duplicated.
template <class P>
static void ConvertPlane( const GenericImage<P>& image, int y0, int c, uint64_t n, float* dst )
{
   const typename P::sample* src = image.ScanLine( y0, c );
   for ( uint64_t i = 0; i < n; ++i )
      P::FromSample( dst[i], src[i] );
}

// ----------------------------------------------------------------------------

bool ViewPanelSource::fill_band( uint32_t panel_id, uint64_t y0, uint64_t y1, float* dst )
{
   if ( panel_id >= m_views.Length() )
      return false;

   // const alias: use the const operator*/ScanLine overloads so reading a
   // shared core image never triggers a private duplication.
   const ImageVariant image = m_views[panel_id].Image();
   if ( !image )
      return false;
   if ( image.IsComplexSample() )
      return false;

   const int w  = image.Width();
   const int h  = image.Height();
   const int ch = image.NumberOfChannels();

   if ( y1 < y0 || y1 > uint64_t( h ) )
      return false;

   const uint64_t rows  = y1 - y0;
   const uint64_t plane = rows * uint64_t( w );   // samples per channel in this band
   const int      iy0   = int( y0 );

   for ( int c = 0; c < ch; ++c )
   {
      // Planar layout: index(c,r,x) = c*rows*w + r*w + x, so channel c starts
      // at dst + c*plane (PROTOCOL.md section 7).
      float* out = dst + uint64_t( c ) * plane;

      if ( image.IsFloatSample() )
      {
         switch ( image.BitsPerSample() )
         {
         case 32:
            {
               // pcl::Image (Float32): rows are contiguous within a channel
               // plane, so rows [y0,y1) are one memcpy from ScanLine(y0,c).
               const Image& img = static_cast<const Image&>( *image );
               std::memcpy( out, img.ScanLine( iy0, c ), size_t( plane ) * sizeof( float ) );
            }
            break;
         case 64:
            ConvertPlane<DoublePixelTraits>( static_cast<const DImage&>( *image ), iy0, c, plane, out );
            break;
         default:
            return false;
         }
      }
      else
      {
         switch ( image.BitsPerSample() )
         {
         case 8:
            ConvertPlane<UInt8PixelTraits>( static_cast<const UInt8Image&>( *image ), iy0, c, plane, out );
            break;
         case 16:
            ConvertPlane<UInt16PixelTraits>( static_cast<const UInt16Image&>( *image ), iy0, c, plane, out );
            break;
         case 32:
            ConvertPlane<UInt32PixelTraits>( static_cast<const UInt32Image&>( *image ), iy0, c, plane, out );
            break;
         default:
            return false;
         }
      }
   }

   return true;
}

// ----------------------------------------------------------------------------

} // namespace pcl
