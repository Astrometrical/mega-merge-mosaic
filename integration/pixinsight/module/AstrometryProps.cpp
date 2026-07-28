// AstrometryProps.cpp -- see AstrometryProps.h.

#include "AstrometryProps.h"

#include <cmath>
#include <cstdint>
#include <string>

#include <pcl/Matrix.h>
#include <pcl/Property.h>
#include <pcl/Variant.h>
#include <pcl/Vector.h>

namespace pcl
{

using nlohmann::json;

// ----------------------------------------------------------------------------

// Map one PCL Variant to a (type_, PropertyValue-JSON) pair. Returns false to
// SKIP the property: an unsupported Variant type, or any value carrying a
// non-finite float (which the worker's Init decoder would reject outright --
// PROTOCOL.md section 6). The externally-tagged PropertyValue JSON shapes match
// crates/mmm-core/src/formats/mod.rs:
//   Str    -> {"Str": string}
//   F64    -> {"F64": number}
//   I64    -> {"I64": integer}
//   F64Vec -> {"F64Vec": [numbers]}
//   F64Mat -> {"F64Mat": {"rows": u32, "cols": u32, "data": [numbers, row-major]}}
static bool VariantToPropertyValue( const Variant& v, std::string& type_, json& value )
{
   switch ( v.Type() )
   {
   // --- strings -------------------------------------------------------------
   case VariantType::String:
      value = json{ { "Str", std::string( v.ToString().ToUTF8().c_str() ) } };
      type_ = "String";
      return true;
   case VariantType::IsoString:
      value = json{ { "Str", std::string( v.ToIsoString().c_str() ) } };
      type_ = "String";
      return true;
   case VariantType::TimePoint:
      // ISO 8601 text, exactly as the worker's "TimePoint" property parser expects.
      value = json{ { "Str", std::string( v.ToString().ToUTF8().c_str() ) } };
      type_ = "TimePoint";
      return true;

   // --- booleans / integers -> I64 ------------------------------------------
   case VariantType::Bool:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } };
      type_ = "Boolean";
      return true;
   case VariantType::Int8:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "Int8";  return true;
   case VariantType::Int16:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "Int16"; return true;
   case VariantType::Int32:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "Int32"; return true;
   case VariantType::Int64:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "Int64"; return true;
   case VariantType::UInt8:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "UInt8";  return true;
   case VariantType::UInt16:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "UInt16"; return true;
   case VariantType::UInt32:
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "UInt32"; return true;
   case VariantType::UInt64:
      // Stored as I64 (the worker's decoded form); astrometric uint64 values
      // are small, so the reinterpretation is lossless in practice.
      value = json{ { "I64", std::int64_t( v.ToInt64() ) } }; type_ = "UInt64"; return true;

   // --- floating-point scalars -> F64 ---------------------------------------
   case VariantType::Float32:
   case VariantType::Float64:
      {
         const double d = v.ToDouble();
         if ( !std::isfinite( d ) )
            return false;
         value = json{ { "F64", d } };
         type_ = ( v.Type() == VariantType::Float32 ) ? "Float32" : "Float64";
         return true;
      }

   // --- real vectors -> F64Vec ----------------------------------------------
   case VariantType::I8Vector:
   case VariantType::UI8Vector:
   case VariantType::I16Vector:
   case VariantType::UI16Vector:
   case VariantType::I32Vector:
   case VariantType::UI32Vector:
   case VariantType::I64Vector:
   case VariantType::UI64Vector:
   case VariantType::F32Vector:
   case VariantType::F64Vector:
      {
         const Vector vec = v.ToVector();     // pcl::Vector == DVector (double components)
         json data = json::array();
         for ( int i = 0; i < vec.Length(); ++i )
         {
            const double d = vec[i];
            if ( !std::isfinite( d ) )
               return false;
            data.push_back( d );
         }
         value = json{ { "F64Vec", std::move( data ) } };
         type_ = "F64Vector";
         return true;
      }

   // --- real matrices -> F64Mat (row-major) ---------------------------------
   case VariantType::I8Matrix:
   case VariantType::UI8Matrix:
   case VariantType::I16Matrix:
   case VariantType::UI16Matrix:
   case VariantType::I32Matrix:
   case VariantType::UI32Matrix:
   case VariantType::I64Matrix:
   case VariantType::UI64Matrix:
   case VariantType::F32Matrix:
   case VariantType::F64Matrix:
      {
         const Matrix m = v.ToMatrix();       // pcl::Matrix == DMatrix (double elements)
         json data = json::array();
         for ( int r = 0; r < m.Rows(); ++r )
            for ( int c = 0; c < m.Cols(); ++c )
            {
               const double d = m[r][c];
               if ( !std::isfinite( d ) )
                  return false;
               data.push_back( d );
            }
         json mat;
         mat["rows"] = std::uint32_t( m.Rows() );
         mat["cols"] = std::uint32_t( m.Cols() );
         mat["data"] = std::move( data );
         value = json{ { "F64Mat", std::move( mat ) } };
         type_ = "F64Matrix";
         return true;
      }

   // Complex, points, rects, byte arrays, string lists, key-value lists, etc.:
   // not part of a plate solution the worker consumes -- skip.
   default:
      return false;
   }
}

// ----------------------------------------------------------------------------

nlohmann::json extract_astrometry_props( const View& view )
{
   json arr = json::array();

   const PropertyArray properties = view.Properties();
   for ( const Property& p : properties )
   {
      const IsoString& id = p.Id();
      if ( !id.StartsWith( "PCL:AstrometricSolution:" ) )
         continue;

      std::string type_;
      json value;
      if ( !VariantToPropertyValue( p.Value(), type_, value ) )
         continue;

      json prop;
      prop["id"]       = std::string( id.c_str() );
      prop["type_"]    = std::move( type_ );
      prop["value"]    = std::move( value );
      prop["location"] = nullptr;   // view properties are in-memory, never attachment-located
      arr.push_back( std::move( prop ) );
   }

   return arr;
}

// ----------------------------------------------------------------------------

} // namespace pcl
